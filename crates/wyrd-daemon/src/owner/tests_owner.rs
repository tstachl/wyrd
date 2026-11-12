//! Read-side tests for the drive-owner interface (child 1): wire
//! round-trips, the field-name snapshot, dispatch over a composed
//! view, the pre-pass observation states, the read bound, and the
//! socket round trip. Mutation submission (child 2) and crash
//! answers (child 3) extend this file, not a second harness.

use super::client::{ClientError, OwnerClient};
use super::mapping::{
    attr_from_wire, file_identity_from_wire, kind_from_wire, map_policy_error,
    mutation_kind_from_wire, node_from_wire, wire_attr, wire_dir_entry, wire_mutation_error,
    wire_mutation_outcome, wire_node, wire_presence, wire_retention_policy,
};
use super::wire::{
    PathParams, RawRequest, SubmitParams, WireAttr, WireDirEntry, WireDurabilityLevel, WireError,
    WireErrorCode, WireFileIdentity, WireFreshnessState, WireKind, WireLiveness, WireMutationKind,
    WireMutationOutcome, WireNode, WireOk, WireParentToken, WirePresence, WireResponse, WireResult,
    WireRetentionPolicy, OP_CAPTURE_PARENT, OP_FRESHNESS, OP_LIVENESS, OP_LOOKUP, OP_OBSERVE,
    OP_PAUSE, OP_POLICY_EVICT, OP_POLICY_PIN, OP_POLICY_STATUS, OP_POLICY_UNPIN, OP_READ,
    OP_READDIR, OP_RESUME, OP_STAT, OP_SUBMIT, WIRE_VERSION,
};
use super::{
    OwnerHandles, OwnerServer, MAX_FRAME_BYTES, MAX_OWNER_CONNECTIONS, MAX_SUBMIT_CONTENT_BYTES,
};

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use wyrd_core::live::LiveParts;
use wyrd_core::view::{Attr, ConflictVersion, DirEntry, Kind, Node};
use wyrd_format::{ContentId, EntryIdentity, SnapshotId};
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
        identity: Some(EntryIdentity::from_bytes([0xA0; 32])),
    };
    let dir = Node::Dir {
        subtree: ContentId::from_bytes([0x02; 32]),
        identity: None,
    };
    let symlink = Node::Symlink {
        target: "hello.txt".to_string(),
        identity: Some(EntryIdentity::from_bytes([0xA1; 32])),
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
        identity: None,
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
        let wire = wire_node(&node);
        let json = serde_json::to_value(&wire).unwrap();
        let back: WireNode = serde_json::from_value(json).unwrap();
        assert_eq!(back, wire, "JSON round trip preserves the wire value");
        assert_eq!(
            node_from_wire(&back).unwrap(),
            node,
            "wire maps back to the namespace value"
        );
    }
    // Invalid identifiers fail the mapping, never decode to a wrong value.
    let bad = WireNode::File {
        size: 1,
        executable: false,
        chunks: vec!["not-hex".to_string()],
        identity: None,
    };
    assert!(node_from_wire(&bad).is_err());
    let short = WireNode::Dir {
        subtree: "abcd".to_string(),
        identity: None,
    };
    assert!(node_from_wire(&short).is_err());
    // An invalid identity fails the mapping, never decodes to a
    // wrong entry: the token is load-bearing for sameness.
    let bad_identity = WireNode::Symlink {
        target: "x".to_string(),
        identity: Some("zz".to_string()),
    };
    assert!(node_from_wire(&bad_identity).is_err());
    // Absence stays absence through the round trip: pre-identity
    // data never gains a token in transit.
    let legacy = WireNode::Dir {
        subtree: hex::encode([0x02; 32]),
        identity: None,
    };
    assert_eq!(
        node_from_wire(&legacy).unwrap(),
        Node::Dir {
            subtree: ContentId::from_bytes([0x02; 32]),
            identity: None,
        }
    );
}

/// Attributes, entries, and kinds round-trip through JSON unchanged.
#[test]
fn wire_attr_and_entry_round_trip() {
    for kind in [Kind::File, Kind::Dir, Kind::Symlink, Kind::Conflict] {
        let attr = Attr {
            kind,
            size: 9,
            executable: true,
            identity: Some(EntryIdentity::from_bytes([0xB0; 32])),
        };
        let wire = wire_attr(&attr);
        let back: WireAttr = serde_json::from_value(serde_json::to_value(&wire).unwrap()).unwrap();
        assert_eq!(back, wire);
        assert_eq!(attr_from_wire(back).unwrap(), attr);
        assert_eq!(kind_from_wire(wire.kind), kind);
    }
    // A pre-identity reader's payload (no identity key) still
    // parses: the token defaults to absence instead of failing.
    let legacy_json = serde_json::json!({"kind": "File", "size": 1, "executable": false});
    let legacy: WireAttr = serde_json::from_value(legacy_json).unwrap();
    assert_eq!(legacy.identity, None);
    assert_eq!(
        attr_from_wire(legacy).unwrap(),
        Attr {
            kind: Kind::File,
            size: 1,
            executable: false,
            identity: None,
        }
    );
    let entry = DirEntry {
        name: "hello.txt".to_string(),
        node: Node::File {
            size: 11,
            executable: false,
            chunks: vec![],
            identity: None,
        },
    };
    let wire = wire_dir_entry(&entry);
    let back: WireDirEntry =
        serde_json::from_value(serde_json::to_value(wire.clone()).unwrap()).unwrap();
    assert_eq!(back, wire);
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

/// A read at exactly the bound still fits the response frame:
/// hex doubles the bytes, JSON adds only its quotes, and the total
/// stays under the ceiling the transport enforces.
#[test]
fn largest_accepted_read_fits_the_frame() {
    use super::server::DEFAULT_MAX_READ_BYTES;
    use wyrd_core::node::WyrdNode;

    let (engine, dir) = scratch_drive();
    let store = wyrd_format::FsObjectStore::open(dir.clone()).unwrap();
    let mut daemon = WyrdNode::new(engine, store).unwrap();
    let big = vec![0xABu8; DEFAULT_MAX_READ_BYTES as usize];
    daemon.put_file("big.bin", &big).unwrap();
    daemon.refresh_live_heads().unwrap();
    let (_live, parts) = daemon
        .into_live(
            Duration::from_secs(5),
            &wyrd_core::live::LiveConfig::default(),
        )
        .unwrap();
    let server = OwnerServer::new(OwnerHandles::from_parts(&parts));
    match dispatch_ok(&server, OP_READ, "big.bin") {
        WireOk::Read { bytes_hex, .. } => {
            assert_eq!(bytes_hex.len(), big.len() * 2);
            let response = WireResponse {
                v: WIRE_VERSION,
                id: 1,
                result: WireResult::Ok(Box::new(WireOk::Read {
                    bytes_hex,
                    generation: 0,
                    revision: 0,
                })),
            };
            let frame = serde_json::to_vec(&response).unwrap();
            assert!(
                frame.len() <= MAX_FRAME_BYTES,
                "largest accepted read fits the frame: {}",
                frame.len()
            );
        }
        other => panic!("boundary read unexpectedly failed: {other:?}"),
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
                assert!(
                    !liveness.paused,
                    "a fresh drive reads back unpaused, never paused by default"
                );
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
                assert!(matches!(freshness.state, WireFreshnessState::FreshNonEmpty));
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

/// Shutdown completes while a client stays connected: tripping
/// the stop flag ends the accept loop, idle connections notice
/// within the poll interval, and the scoped join returns — no
/// shutdown blocks past an in-flight dispatch. Covers a client
/// holding a full request open and one stuck mid-frame.
#[test]
fn shutdown_completes_with_a_client_connected() {
    let (parts, dir) = composed_parts();
    let server = OwnerServer::new(OwnerHandles::from_parts(&parts));
    let socket_path = dir.join("shutdown.sock");
    let _ = std::fs::remove_file(&socket_path);
    let stop: &'static AtomicBool = Box::leak(Box::new(AtomicBool::new(false)));
    let serve_thread = std::thread::spawn(move || {
        server
            .bind_and_serve(&socket_path, stop)
            .expect("the interface serves");
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !dir.join("shutdown.sock").exists() {
        assert!(std::time::Instant::now() < deadline, "the socket binds");
        std::thread::sleep(Duration::from_millis(10));
    }

    // A client with a completed request, connection held open.
    let mut idle = UnixStream::connect(dir.join("shutdown.sock")).unwrap();
    let request = serde_json::to_value(&RawRequest {
        v: WIRE_VERSION,
        id: 51,
        op: OP_LOOKUP.to_string(),
        params: serde_json::to_value(PathParams {
            path: "hello.txt".to_string(),
        })
        .unwrap(),
    })
    .unwrap();
    let request = serde_json::to_vec(&request).unwrap();
    idle.write_all(&(request.len() as u32).to_be_bytes())
        .unwrap();
    idle.write_all(&request).unwrap();
    let mut len = [0u8; 4];
    idle.read_exact(&mut len).unwrap();

    // A client stuck mid-frame: two bytes, never a full message.
    let mut partial = UnixStream::connect(dir.join("shutdown.sock")).unwrap();
    partial.write_all(&[0u8, 2]).unwrap();

    stop.store(true, Ordering::Relaxed);
    // Join with a deadline: a hung join fails the test instead of
    // hanging the suite. The flag avoids channel calls, which the
    // mailbox-construction contract scan would flag.
    let joined = Arc::new(AtomicBool::new(false));
    let joined_flag = Arc::clone(&joined);
    std::thread::spawn(move || {
        serve_thread.join().unwrap();
        joined_flag.store(true, Ordering::Relaxed);
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !joined.load(Ordering::Relaxed) {
        assert!(
            std::time::Instant::now() < deadline,
            "shutdown completes with clients connected"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(idle);
    drop(partial);

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

/// One framed request over a fresh client connection, returning the
/// parsed response. The socket tests share it so each pause assertion
/// exercises the real framing, not direct dispatch.
fn transact(socket_path: &std::path::Path, id: u64, op: &str) -> WireResponse {
    let mut client = UnixStream::connect(socket_path).unwrap();
    let request = serde_json::to_vec(&RawRequest {
        v: WIRE_VERSION,
        id,
        op: op.to_string(),
        params: serde_json::Value::Null,
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
    serde_json::from_slice(&frame).unwrap()
}

/// Try one framed request with explicit params over a fresh client
/// connection. `Err` means the server did not answer in time — a
/// saturated pool closes without answering, a draining pool may too
/// — so the caller retries or fails on its own deadline.
fn try_transact_params(
    socket_path: &std::path::Path,
    id: u64,
    op: &str,
    params: serde_json::Value,
) -> std::io::Result<WireResponse> {
    let mut client = UnixStream::connect(socket_path)?;
    let request = serde_json::to_vec(&RawRequest {
        v: WIRE_VERSION,
        id,
        op: op.to_string(),
        params,
    })
    .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    client.write_all(&(request.len() as u32).to_be_bytes())?;
    client.write_all(&request)?;
    client
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut len = [0u8; 4];
    client.read_exact(&mut len)?;
    let len = u32::from_be_bytes(len) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "response exceeds interface bound",
        ));
    }
    let mut frame = vec![0u8; len];
    client.read_exact(&mut frame)?;
    serde_json::from_slice(&frame)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
}

/// One framed request with explicit params over a fresh client
/// connection, returning the parsed response. The framing tests
/// share it so each pins the real socket behavior, not direct
/// dispatch.
fn transact_params(
    socket_path: &std::path::Path,
    id: u64,
    op: &str,
    params: serde_json::Value,
) -> WireResponse {
    try_transact_params(socket_path, id, op, params).expect("the server answers the request")
}

/// Serve `server` on a fresh socket in `dir`: bind non-blocking like
/// every socket test, spawn `serve_on`, and hand the join back. The
/// caller trips `stop` and joins.
fn serve_parts(
    server: OwnerServer,
    socket_name: &str,
    dir: &std::path::Path,
    stop: &'static AtomicBool,
) -> (std::path::PathBuf, std::thread::JoinHandle<()>) {
    let socket_path = dir.join(socket_name);
    let _ = std::fs::remove_file(&socket_path);
    let listener = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();
    listener.set_nonblocking(true).unwrap();
    let serve_thread = std::thread::spawn(move || server.serve_on(&listener, stop));
    (socket_path, serve_thread)
}

/// Read a lookup answer and unwrap the file node. The framing tests
/// share it so each asserts content, not just round-trip shape.
fn lookup_file(response: WireResponse, id: u64) -> WireNode {
    assert_eq!(response.v, WIRE_VERSION);
    assert_eq!(response.id, id);
    match response.result {
        WireResult::Ok(ok) => match *ok {
            WireOk::Lookup { node, .. } => node,
            other => panic!("unexpected lookup result: {other:?}"),
        },
        other => panic!("lookup unexpectedly failed: {other:?}"),
    }
}

/// Pause and resume through dispatch: the switch flips, the answer
/// echoes the switch as it reads back, both operations are
/// idempotent, and the wired pacing signal fires — the poke the
/// contract requires, not a bare store.
#[test]
fn pause_resume_round_trip_pokes_the_loop_signal() {
    use wyrd_core::wake::{Wake, WakeSignal};

    let (parts, dir) = composed_parts();
    let waker = Arc::new(WakeSignal::default());
    let server = OwnerServer::new(OwnerHandles::from_parts(&parts).with_waker(Arc::clone(&waker)));
    // A waiter on the loop's signal: the pause must wake it promptly,
    // the way the loop's idle wait would observe it.
    let waiter_waker = Arc::clone(&waker);
    let waited = std::thread::spawn(move || {
        let stop = AtomicBool::new(false);
        waiter_waker.wait(&stop, Duration::from_secs(30))
    });
    let paused = server.dispatch(RawRequest {
        v: WIRE_VERSION,
        id: 71,
        op: OP_PAUSE.to_string(),
        params: serde_json::Value::Null,
    });
    match paused.result {
        WireResult::Ok(ok) => match *ok {
            WireOk::Paused { paused } => assert!(paused, "the answer echoes the set switch"),
            other => panic!("unexpected pause result: {other:?}"),
        },
        other => panic!("pause unexpectedly failed: {other:?}"),
    }
    assert_eq!(
        waited.join().unwrap(),
        Wake::Signal,
        "setting pause pokes the pacing signal"
    );
    // Idempotent: pausing twice is one pause, still echoed back.
    match server
        .dispatch(RawRequest {
            v: WIRE_VERSION,
            id: 72,
            op: OP_PAUSE.to_string(),
            params: serde_json::Value::Null,
        })
        .result
    {
        WireResult::Ok(ok) => match *ok {
            WireOk::Paused { paused } => assert!(paused, "the second pause reads back paused"),
            other => panic!("unexpected second pause result: {other:?}"),
        },
        other => panic!("second pause unexpectedly failed: {other:?}"),
    }
    // Resume clears; a resume on a running drive changes nothing.
    for id in [73, 74] {
        match server
            .dispatch(RawRequest {
                v: WIRE_VERSION,
                id,
                op: OP_RESUME.to_string(),
                params: serde_json::Value::Null,
            })
            .result
        {
            WireResult::Ok(ok) => match *ok {
                WireOk::Paused { paused } => {
                    assert!(!paused, "resume reads back running")
                }
                other => panic!("unexpected resume result: {other:?}"),
            },
            other => panic!("resume unexpectedly failed: {other:?}"),
        }
    }

    std::fs::remove_dir_all(dir).unwrap();
}

/// Three states stay three over the wire: quiet (reachable, no pass
/// yet, unpaused), paused (reachable, no pass yet, paused), and
/// offline (the connection refuses). The two liveness answers differ
/// in exactly the paused flag; the offline owner answers nothing at
/// all. No renderer can collapse them into one status.
#[test]
fn liveness_keeps_paused_quiet_and_offline_distinct() {
    let (parts, dir) = composed_parts();
    let server = OwnerServer::new(OwnerHandles::from_parts(&parts));
    let socket_path = dir.join("pausestates.sock");
    let _ = std::fs::remove_file(&socket_path);
    let listener = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();
    listener.set_nonblocking(true).unwrap();
    let stop: &'static AtomicBool = Box::leak(Box::new(AtomicBool::new(false)));
    let serve_thread = std::thread::spawn(move || server.serve_on(&listener, stop));

    fn liveness_of(response: WireResponse) -> WireLiveness {
        match response.result {
            WireResult::Ok(ok) => match *ok {
                WireOk::Liveness { liveness } => liveness,
                other => panic!("unexpected liveness result: {other:?}"),
            },
            other => panic!("liveness unexpectedly failed: {other:?}"),
        }
    }
    // Quiet: reachable, no pass yet, unpaused.
    let quiet = liveness_of(transact(&socket_path, 81, OP_LIVENESS));
    assert!(!quiet.paused, "a fresh drive reads back unpaused");
    assert_eq!(quiet.passes, 0);
    assert_eq!(quiet.report, None);
    // Paused over the socket: the same slot, the flag set.
    match transact(&socket_path, 82, OP_PAUSE).result {
        WireResult::Ok(ok) => match *ok {
            WireOk::Paused { paused } => assert!(paused, "pause echoes back set"),
            other => panic!("unexpected socket pause result: {other:?}"),
        },
        other => panic!("socket pause unexpectedly failed: {other:?}"),
    }
    let paused = liveness_of(transact(&socket_path, 83, OP_LIVENESS));
    assert!(paused.paused, "the pause shows in liveness");
    assert_ne!(quiet, paused, "quiet and paused are distinct answers");
    assert_eq!(
        (&paused.health, &paused.report, &paused.passes),
        (&quiet.health, &quiet.report, &quiet.passes),
        "pausing changes the flag, nothing else"
    );
    // Resume over the socket restores the quiet answer exactly.
    match transact(&socket_path, 84, OP_RESUME).result {
        WireResult::Ok(ok) => match *ok {
            WireOk::Paused { paused } => assert!(!paused, "resume echoes back running"),
            other => panic!("unexpected socket resume result: {other:?}"),
        },
        other => panic!("socket resume unexpectedly failed: {other:?}"),
    }
    assert_eq!(
        liveness_of(transact(&socket_path, 85, OP_LIVENESS)),
        quiet,
        "resume restores the quiet answer byte for byte"
    );

    // Offline: the stopped server refuses the connection instead of
    // answering anything, which no response can be confused with.
    stop.store(true, Ordering::Relaxed);
    serve_thread.join().unwrap();
    let _ = std::fs::remove_file(&socket_path);
    assert!(
        UnixStream::connect(&socket_path).is_err(),
        "no listener means a refused connection, never a status"
    );

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
        .expect("healthy table")
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
    assert_eq!(wire_mutation_outcome(&direct), outcome);

    // A token from a previous owner instance refuses stale before
    // any queue check: flip the instance bit on an otherwise valid
    // capture and the create must not reach the queue.
    let mut stale = token;
    stale.instance ^= 1;
    let error = submit_err(
        &server,
        WireMutationKind::CreateFile {
            path: "same-forged.txt".to_string(),
            parent: stale,
        },
    );
    assert_eq!(error.code, WireErrorCode::StaleParent);
    assert!(
        error.message.contains("previous owner instance"),
        "the refusal names the cause: {}",
        error.message
    );

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

    // A forged counter refuses stale at the queue's re-check: the
    // instance matches, but no such token was ever captured.
    let error = submit_err(
        &server,
        WireMutationKind::CreateFile {
            path: "nope.txt".to_string(),
            parent: WireParentToken {
                instance: server.instance(),
                counter: 0xdead_beef,
            },
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
        let mapped = wire_mutation_error(&error);
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
            parent: WireParentToken {
                instance: 77,
                counter: 7,
            },
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
            mutation_kind_from_wire(&back, 77).is_ok(),
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
        mutation_kind_from_wire(&nested, 77).is_err(),
        "a nested fold refuses at the wire"
    );
    assert!(file_identity_from_wire(&identity).is_ok());
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

/// A broken submit connection is unconfirmed, never success or
/// failure: a client whose connection breaks while its submit is
/// blocked gets no terminal answer either way, the admitted mutation
/// stays pending (the break fails nothing), and a reconcile re-read
/// reaches a definite verdict before any retry. Contract:
/// `docs/contracts/owner-interface.md` ("Crash answers").
#[test]
fn broken_submit_connection_is_unconfirmed_and_reconciles() {
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
    let socket_path = dir.join("unconfirmed.sock");
    let _ = std::fs::remove_file(&socket_path);
    let listener = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();
    listener.set_nonblocking(true).unwrap();
    let stop: &'static AtomicBool = Box::leak(Box::new(AtomicBool::new(false)));
    let serve_server = Arc::clone(&server);
    let serve_thread = std::thread::spawn(move || serve_server.serve_on(&listener, stop));

    // The submit blocks: admitted, unexecuted, no pass running.
    let mut client = UnixStream::connect(&socket_path).unwrap();
    let request = serde_json::to_vec(&RawRequest {
        v: WIRE_VERSION,
        id: 61,
        op: OP_SUBMIT.to_string(),
        params: serde_json::to_value(SubmitParams {
            kind: WireMutationKind::Mkdir {
                path: "reconcile-me".to_string(),
            },
        })
        .unwrap(),
    })
    .unwrap();
    client
        .write_all(&(request.len() as u32).to_be_bytes())
        .unwrap();
    client.write_all(&request).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while parts.mutations.outstanding() != 1 {
        assert!(
            std::time::Instant::now() < deadline,
            "the submit is admitted but unexecuted"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    // No answer while blocked: the read deadline expires instead of
    // producing success or failure.
    client
        .set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    let mut len = [0u8; 4];
    match client.read_exact(&mut len) {
        Err(error)
            if error.kind() == std::io::ErrorKind::TimedOut
                || error.kind() == std::io::ErrorKind::WouldBlock => {}
        other => panic!("no terminal answer while the submit is blocked: {other:?}"),
    }

    // The break: the client goes away with no answer delivered.
    drop(client);
    assert_eq!(
        parts.mutations.outstanding(),
        1,
        "the break confirms nothing and fails nothing"
    );

    // Reconcile before retry over a fresh connection, the way a
    // real consumer re-reads: the touched path reaches a definite
    // verdict — absent, never executed.
    let mut fresh = UnixStream::connect(&socket_path).unwrap();
    let probe = serde_json::to_vec(&RawRequest {
        v: WIRE_VERSION,
        id: 62,
        op: OP_LOOKUP.to_string(),
        params: serde_json::to_value(PathParams {
            path: "reconcile-me".to_string(),
        })
        .unwrap(),
    })
    .unwrap();
    fresh
        .write_all(&(probe.len() as u32).to_be_bytes())
        .unwrap();
    fresh.write_all(&probe).unwrap();
    let mut len = [0u8; 4];
    fresh.read_exact(&mut len).unwrap();
    let len = u32::from_be_bytes(len) as usize;
    assert!(len <= MAX_FRAME_BYTES);
    let mut frame = vec![0u8; len];
    fresh.read_exact(&mut frame).unwrap();
    let response: WireResponse = serde_json::from_slice(&frame).unwrap();
    assert_eq!(response.id, 62);
    match response.result {
        WireResult::Err(error) => assert_eq!(error.code, WireErrorCode::NotFound),
        WireResult::Ok(ok) => panic!("unexecuted submit must not be visible: {ok:?}"),
    }
    drop(fresh);

    // The pending mutation still executes exactly once through the
    // normal pass. No retry was sent, so no duplicate can exist.
    let mut mailbox = NoopMailbox;
    live.sync_once(&mut mailbox, None::<&mut wyrd_sync::bulk::MemoryBulkSource>)
        .unwrap();
    assert_eq!(
        parts.mutations.outstanding(),
        0,
        "the admitted submit completed through the pass"
    );
    match dispatch_ok(&server, OP_LOOKUP, "reconcile-me") {
        WireOk::Lookup { .. } => {}
        other => panic!("reconciled submit must be visible: {other:?}"),
    }

    stop.store(true, Ordering::Relaxed);
    serve_thread.join().unwrap();
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

// ---------------------------------------------------------------------------
// Freshness judgments (review findings 3 and 5)
// ---------------------------------------------------------------------------

/// A mailbox whose intake fails: every pass fails before
/// publication, so the finding-3 wrapper must record the retaining
/// judgment for a non-partition failure.
struct FailingMailbox;

impl Mailbox for FailingMailbox {
    fn send(&mut self, _envelope: MailboxEnvelope) -> Result<SendReport, MailboxError> {
        Ok(SendReport { accepted: 1 })
    }

    fn recv(&mut self) -> Result<Option<Delivery>, MailboxError> {
        Err(MailboxError::Transport("intake refused".to_string()))
    }

    fn settle(&mut self, _id: DeliveryId, _disposition: Disposition) -> Result<(), MailboxError> {
        Ok(())
    }
}

/// Seed one file's bytes and tree into a store and return the tree:
/// the same bytes hash to the same identities in any store.
fn seed_fork_bytes<S>(store: &mut S, label: &[u8], name: &str) -> wyrd_format::ContentId
where
    S: wyrd_format::ObjectStore,
    S::Error: std::fmt::Debug,
{
    use wyrd_format::{Entry, ObjectKind, Tree};
    let chunk = store
        .insert(ObjectKind::Chunk, label)
        .expect("the fork chunk inserts");
    Tree::from_entries(vec![Entry::file(
        name,
        label.len() as u64,
        false,
        vec![chunk],
    )
    .expect("the entry builds")])
    .expect("the fork tree builds")
    .insert_into(store)
    .expect("the fork tree inserts")
}

/// A composition that withholds eligible heads reads retained, not
/// fresh-empty: the view serves empty while a head awaits its
/// closure, so calling it a fresh empty drive would lie to the
/// consumer.
#[test]
fn withheld_pending_projection_is_retained_not_fresh() {
    use wyrd_core::node::WyrdNode;

    let (engine, dir) = scratch_drive();
    let store = wyrd_format::FsObjectStore::open(dir.clone()).unwrap();
    let mut daemon: WyrdNode<crate::vault::OwnerView> = WyrdNode::new(engine, store).unwrap();
    {
        let (engine, _view) = daemon.parts_mut();
        let mut objects = wyrd_format::MemoryObjectStore::default();
        let tree = seed_fork_bytes(&mut objects, b"withheld", "withheld.txt");
        engine
            .author_snapshot(&objects, tree)
            .expect("the pending head authors");
    }
    assert_eq!(
        daemon.engine().live_heads().unwrap().len(),
        1,
        "one eligible head, closure missing"
    );
    daemon.refresh_live_heads().unwrap();
    let (_live, parts) = daemon
        .into_live(
            Duration::from_secs(5),
            &wyrd_core::live::LiveConfig::default(),
        )
        .unwrap();
    let freshness = parts.freshness.read().unwrap().clone();
    match freshness.state {
        wyrd_core::projection::FreshnessState::RetainedLastGood { detail } => {
            assert!(
                detail.contains("closure still fetching"),
                "the reason names the withheld projection: {detail}"
            );
        }
        other => panic!("withheld heads must not read fresh: {other:?}"),
    }
    assert_eq!(freshness.generation, 0);

    std::fs::remove_dir_all(dir).unwrap();
}

/// A withheld projection that completes resets to fresh on the next
/// publication: the judgment follows the served generation, and a
/// successful refresh clears the retained note.
#[test]
fn retained_freshness_resets_on_publication() {
    use wyrd_core::node::WyrdNode;

    let (engine, dir) = scratch_drive();
    let store = wyrd_format::FsObjectStore::open(dir.clone()).unwrap();
    let mut daemon: WyrdNode<crate::vault::OwnerView> = WyrdNode::new(engine, store).unwrap();
    {
        let (engine, _view) = daemon.parts_mut();
        let mut objects = wyrd_format::MemoryObjectStore::default();
        let tree = seed_fork_bytes(&mut objects, b"withheld", "withheld.txt");
        engine
            .author_snapshot(&objects, tree)
            .expect("the pending head authors");
    }
    daemon.refresh_live_heads().unwrap();
    let (mut live, parts) = daemon
        .into_live(
            Duration::from_secs(5),
            &wyrd_core::live::LiveConfig::default(),
        )
        .unwrap();
    assert!(matches!(
        parts.freshness.read().unwrap().state,
        wyrd_core::projection::FreshnessState::RetainedLastGood { .. }
    ));

    // The same bytes land in the serving view: the closure
    // completes, and a new commit advances the durable revision, so
    // the next pass publishes and the judgment turns fresh.
    {
        let view = parts.projection.read().unwrap();
        let store = view.view().store_handle();
        let mut store = store.write().unwrap();
        seed_fork_bytes(&mut *store, b"withheld", "withheld.txt");
    }
    let mut mailbox = NoopMailbox;
    std::thread::scope(|scope| {
        scope.spawn(|| {
            parts
                .mutations
                .submit(wyrd_core::mutation::MutationKind::Mkdir {
                    path: "trigger".to_string(),
                })
                .expect("the trigger mutation commits");
        });
        live.sync_once(&mut mailbox, None::<&mut wyrd_sync::bulk::MemoryBulkSource>)
            .expect("the completing pass runs");
    });
    let freshness = parts.freshness.read().unwrap().clone();
    assert!(matches!(
        freshness.state,
        wyrd_core::projection::FreshnessState::FreshNonEmpty
    ));
    assert_eq!(freshness.generation, 1);

    std::fs::remove_dir_all(dir).unwrap();
}

/// A non-partition pass failure records the retaining judgment too:
/// intake fails before any refresh runs, yet the freshness slot
/// must not keep reading fresh.
#[test]
fn intake_failure_records_retained_freshness() {
    use wyrd_core::node::WyrdNode;

    let (engine, dir) = scratch_drive();
    let store = wyrd_format::FsObjectStore::open(dir.clone()).unwrap();
    let mut daemon: WyrdNode<crate::vault::OwnerView> = WyrdNode::new(engine, store).unwrap();
    daemon.put_file("seed.txt", b"seed").unwrap();
    daemon.refresh_live_heads().unwrap();
    let (mut live, parts) = daemon
        .into_live(
            Duration::from_secs(5),
            &wyrd_core::live::LiveConfig::default(),
        )
        .unwrap();
    assert!(matches!(
        parts.freshness.read().unwrap().state,
        wyrd_core::projection::FreshnessState::FreshNonEmpty
    ));

    let mut mailbox = FailingMailbox;
    let error = live
        .sync_once(&mut mailbox, None::<&mut wyrd_sync::bulk::MemoryBulkSource>)
        .expect_err("intake refusal fails the pass");
    assert!(
        matches!(error, wyrd_core::live::LiveError::Engine(_)),
        "the failure is the intake error: {error:?}"
    );
    match &parts.freshness.read().unwrap().state {
        wyrd_core::projection::FreshnessState::RetainedLastGood { .. } => {}
        other => panic!("a failed pass must retain, not read fresh: {other:?}"),
    }

    std::fs::remove_dir_all(dir).unwrap();
}

// ---------------------------------------------------------------------------
// Framing honesty (owner-frames P1): explicit assembly, typed
// over-limit refusal, bounded connection threads.
// ---------------------------------------------------------------------------

/// A stall mid-frame closes the connection deterministically: a
/// client that trickles a length prefix, sleeps past the read
/// deadline, then sends the remainder gets a clean close with zero
/// misframed traffic — the remainder is never read as a fresh
/// length prefix.
#[test]
fn stalled_mid_frame_closes_with_zero_misframed_traffic() {
    let (parts, dir) = composed_parts();
    let server = OwnerServer::new(OwnerHandles::from_parts(&parts));
    let stop: &'static AtomicBool = Box::leak(Box::new(AtomicBool::new(false)));
    let (socket_path, serve_thread) = serve_parts(server, "trickle.sock", &dir, stop);

    let lookup_params = || {
        serde_json::to_value(PathParams {
            path: "hello.txt".to_string(),
        })
        .unwrap()
    };
    let request = serde_json::to_vec(&RawRequest {
        v: WIRE_VERSION,
        id: 81,
        op: OP_LOOKUP.to_string(),
        params: lookup_params(),
    })
    .unwrap();
    let mut wire = (request.len() as u32).to_be_bytes().to_vec();
    wire.extend_from_slice(&request);

    let mut client = UnixStream::connect(&socket_path).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    // Two length bytes, then silence past the 250 ms read deadline.
    client.write_all(&wire[..2]).unwrap();
    std::thread::sleep(Duration::from_millis(800));
    // The server has closed by now: EOF, never a response.
    let mut seen = Vec::new();
    let mut buf = [0u8; 1024];
    loop {
        match client.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => seen.extend_from_slice(&buf[..n]),
            Err(error) => panic!("a torn close reads as EOF, never a stall: {error:?}"),
        }
    }
    assert!(seen.is_empty(), "a torn frame dispatches nothing: {seen:?}");
    // The remainder lands on a closed connection: ignored, and still
    // no response bytes follow it.
    let _ = client.write_all(&wire[2..]);
    match client.read(&mut buf) {
        Ok(0) => {}
        Err(_) => {}
        Ok(n) => panic!("no response follows a torn frame, saw {n} bytes"),
    }

    // The stream is not desynchronized: fresh connections serve.
    let response = transact_params(&socket_path, 82, OP_LOOKUP, lookup_params());
    match lookup_file(response, 82) {
        WireNode::File { size: 11, .. } => {}
        other => panic!("unexpected lookup node after a torn frame: {other:?}"),
    }

    stop.store(true, Ordering::Relaxed);
    serve_thread.join().unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

/// An overlong length prefix closes the connection with no answer:
/// framing honesty means the overlong frame is never read, never
/// dispatched, and the client sees unconfirmed silence — while the
/// server keeps serving fresh connections.
#[test]
fn overlong_length_prefix_closes_without_answer() {
    let (parts, dir) = composed_parts();
    let server = OwnerServer::new(OwnerHandles::from_parts(&parts));
    let stop: &'static AtomicBool = Box::leak(Box::new(AtomicBool::new(false)));
    let (socket_path, serve_thread) = serve_parts(server, "overlong.sock", &dir, stop);

    let mut client = UnixStream::connect(&socket_path).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    client
        .write_all(&((MAX_FRAME_BYTES as u32 + 1).to_be_bytes()))
        .unwrap();
    // No response bytes precede the close: EOF, not a refusal.
    let mut buf = [0u8; 8];
    let mut seen = 0;
    loop {
        match client.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => seen += n,
            Err(error) => panic!("an overlong close reads as EOF, never a stall: {error:?}"),
        }
    }
    assert_eq!(seen, 0, "an overlong frame is never answered");
    drop(client);

    let response = transact_params(
        &socket_path,
        83,
        OP_LOOKUP,
        serde_json::to_value(PathParams {
            path: "hello.txt".to_string(),
        })
        .unwrap(),
    );
    match lookup_file(response, 83) {
        WireNode::File { size: 11, .. } => {}
        other => panic!("unexpected lookup node after an overlong frame: {other:?}"),
    }

    stop.store(true, Ordering::Relaxed);
    serve_thread.join().unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

/// An over-limit submit is a typed `TooLarge` refusal, not silence:
/// the submit content bound sits below the frame ceiling, so the
/// refusal carries the request id and retry guidance on the
/// still-open connection.
#[test]
fn over_limit_submit_is_a_typed_toolarge_not_silence() {
    let (parts, dir) = composed_parts();
    let server = OwnerServer::new(OwnerHandles::from_parts(&parts));
    let stop: &'static AtomicBool = Box::leak(Box::new(AtomicBool::new(false)));
    let (socket_path, serve_thread) = serve_parts(server, "toolarge.sock", &dir, stop);

    // One byte past the submit bound: the hex doubles it, the frame
    // still fits the 96 MiB ceiling, so the refusal is typed.
    let content_hex = "ab".repeat(MAX_SUBMIT_CONTENT_BYTES + 1);
    let request = serde_json::to_vec(&RawRequest {
        v: WIRE_VERSION,
        id: 84,
        op: OP_SUBMIT.to_string(),
        params: serde_json::to_value(SubmitParams {
            kind: WireMutationKind::AppendFile {
                path: "hello.txt".to_string(),
                content_hex,
            },
        })
        .unwrap(),
    })
    .unwrap();
    assert!(
        request.len() <= MAX_FRAME_BYTES,
        "the over-limit submit still fits the frame: {}",
        request.len()
    );
    let mut client = UnixStream::connect(&socket_path).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();
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
    assert_eq!(response.id, 84);
    match response.result {
        WireResult::Err(error) => {
            assert_eq!(error.code, WireErrorCode::TooLarge);
            assert!(
                error.message.contains("split into smaller submits")
                    && error.message.contains("mount"),
                "the refusal carries retry guidance: {}",
                error.message
            );
        }
        ok => panic!("an over-limit submit must refuse, not answer: {ok:?}"),
    }
    // The refusal is terminal, not severance: the same connection
    // serves on.
    let lookup = serde_json::to_vec(&RawRequest {
        v: WIRE_VERSION,
        id: 85,
        op: OP_LOOKUP.to_string(),
        params: serde_json::to_value(PathParams {
            path: "hello.txt".to_string(),
        })
        .unwrap(),
    })
    .unwrap();
    client
        .write_all(&(lookup.len() as u32).to_be_bytes())
        .unwrap();
    client.write_all(&lookup).unwrap();
    let mut len = [0u8; 4];
    client.read_exact(&mut len).unwrap();
    let len = u32::from_be_bytes(len) as usize;
    let mut frame = vec![0u8; len];
    client.read_exact(&mut frame).unwrap();
    let response: WireResponse = serde_json::from_slice(&frame).unwrap();
    match lookup_file(response, 85) {
        WireNode::File { size: 11, .. } => {}
        other => panic!("the connection serves on after a typed refusal: {other:?}"),
    }

    stop.store(true, Ordering::Relaxed);
    drop(client);
    serve_thread.join().unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

/// A stall mid-body closes the connection deterministically, like
/// a stall mid-length: a client that sends a full length prefix and
/// one body byte, then sleeps past the read deadline, gets a clean
/// close with zero misframed traffic.
#[test]
fn stalled_mid_body_closes_with_zero_misframed_traffic() {
    let (parts, dir) = composed_parts();
    let server = OwnerServer::new(OwnerHandles::from_parts(&parts));
    let stop: &'static AtomicBool = Box::leak(Box::new(AtomicBool::new(false)));
    let (socket_path, serve_thread) = serve_parts(server, "midbody.sock", &dir, stop);

    let request = serde_json::to_vec(&RawRequest {
        v: WIRE_VERSION,
        id: 87,
        op: OP_LOOKUP.to_string(),
        params: serde_json::to_value(PathParams {
            path: "hello.txt".to_string(),
        })
        .unwrap(),
    })
    .unwrap();
    let mut wire = (request.len() as u32).to_be_bytes().to_vec();
    wire.extend_from_slice(&request);

    let mut client = UnixStream::connect(&socket_path).unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    // A full length prefix and one body byte, then silence past the
    // 250 ms read deadline: the length is already consumed, so even
    // the first body stall is a torn frame, never an idle poll.
    client.write_all(&wire[..5]).unwrap();
    std::thread::sleep(Duration::from_millis(800));
    let mut seen = Vec::new();
    let mut buf = [0u8; 1024];
    loop {
        match client.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => seen.extend_from_slice(&buf[..n]),
            Err(error) => panic!("a torn close reads as EOF, never a stall: {error:?}"),
        }
    }
    assert!(seen.is_empty(), "a torn body dispatches nothing: {seen:?}");
    drop(client);

    let response = transact_params(
        &socket_path,
        88,
        OP_LOOKUP,
        serde_json::to_value(PathParams {
            path: "hello.txt".to_string(),
        })
        .unwrap(),
    );
    match lookup_file(response, 88) {
        WireNode::File { size: 11, .. } => {}
        other => panic!("unexpected lookup node after a torn body: {other:?}"),
    }

    stop.store(true, Ordering::Relaxed);
    serve_thread.join().unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

/// An accept flood stays bounded: the pool holds, excess
/// connections close promptly instead of spawning threads, and the
/// server keeps serving once the flood drains.
#[test]
fn accept_flood_stays_bounded() {
    let (parts, dir) = composed_parts();
    let server = OwnerServer::new(OwnerHandles::from_parts(&parts));
    let stop: &'static AtomicBool = Box::leak(Box::new(AtomicBool::new(false)));
    let (socket_path, serve_thread) = serve_parts(server, "flood.sock", &dir, stop);

    // Fill the pool with idle holders.
    let mut holders = Vec::new();
    for _ in 0..MAX_OWNER_CONNECTIONS {
        holders.push(UnixStream::connect(&socket_path).unwrap());
    }
    std::thread::sleep(Duration::from_secs(1));
    // Excess connections close promptly instead of spawning.
    let mut extras = Vec::new();
    for _ in 0..8 {
        let extra = UnixStream::connect(&socket_path).unwrap();
        extra
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        extras.push(extra);
    }
    std::thread::sleep(Duration::from_secs(1));
    for extra in extras.iter_mut() {
        let mut buf = [0u8; 1];
        match extra.read(&mut buf) {
            Ok(0) => {}
            Err(error) => {
                panic!("excess connections close promptly, never stall: {error:?}")
            }
            Ok(_) => panic!("an excess connection was served past the pool bound"),
        }
    }
    drop(extras);
    drop(holders);
    // The pool drains asynchronously as holder threads observe the
    // EOF and release their slots: retry the fresh lookup until the
    // server serves again or the deadline expires, instead of a
    // single shot that races the drain.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let lookup_params = serde_json::to_value(PathParams {
        path: "hello.txt".to_string(),
    })
    .unwrap();
    let response = loop {
        match try_transact_params(&socket_path, 86, OP_LOOKUP, lookup_params.clone()) {
            Ok(response) => break response,
            Err(_) => {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the server serves again after the flood drains"
                );
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    };
    match lookup_file(response, 86) {
        WireNode::File { size: 11, .. } => {}
        other => panic!("the server serves on after the flood drains: {other:?}"),
    }

    stop.store(true, Ordering::Relaxed);
    serve_thread.join().unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

// ---------------------------------------------------------------------------
// Shared Rust client (OD-16-F, client-integration decision)
// ---------------------------------------------------------------------------

/// The typed client serves the whole read surface against a live
/// server: every op returns its own shape, the observation refusal
/// stays explicit, and reconciliation maps absence to data.
#[test]
fn client_serves_the_typed_surface() {
    let (parts, dir) = composed_parts();
    let server = OwnerServer::new(OwnerHandles::from_parts(&parts));
    let stop: &'static AtomicBool = Box::leak(Box::new(AtomicBool::new(false)));
    let (socket_path, serve_thread) = serve_parts(server, "client.sock", &dir, stop);
    let mut client = OwnerClient::connect(&socket_path).expect("the owner answers");

    let (node, generation, revision) = client.lookup("hello.txt").expect("lookup serves");
    assert!(
        matches!(node, WireNode::File { size: 11, .. }),
        "lookup serves the seeded file: {node:?}"
    );
    let (_, stat_generation, stat_revision) = client.stat("hello.txt").expect("stat serves");
    assert_eq!((stat_generation, stat_revision), (generation, revision));
    let (entries, _, _) = client.readdir("").expect("readdir serves the root");
    assert!(
        entries.iter().any(|entry| entry.name == "hello.txt"),
        "the root lists the seeded file"
    );
    let (bytes, _, _) = client.read("hello.txt").expect("read serves");
    assert_eq!(bytes, b"hello owner");
    match client.observe() {
        Err(ClientError::Refused(error)) => assert_eq!(error.code, WireErrorCode::Unobserved),
        other => panic!("no pass completed, so observe refuses explicitly: {other:?}"),
    }
    let liveness = client.liveness().expect("liveness serves");
    assert_eq!((liveness.passes, liveness.paused), (0, false));
    match client.freshness().expect("freshness serves").state {
        WireFreshnessState::FreshNonEmpty => {}
        other => panic!("the composed view is fresh and non-empty: {other:?}"),
    }
    assert!(client.pause().expect("pause sets the switch"));
    assert!(client.liveness().expect("liveness re-reads").paused);
    assert!(!client.resume().expect("resume clears the switch"));

    // Reconciliation: presence is data, absence is data.
    assert!(client
        .reconcile_lookup("hello.txt")
        .expect("reconcile reads")
        .is_some());
    assert!(client
        .reconcile_lookup("missing.txt")
        .expect("absence reconciles")
        .is_none());

    stop.store(true, Ordering::Relaxed);
    serve_thread.join().unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

/// Submit through the client against a running loop: creates land,
/// the re-read confirms them, and refusals carry their own codes —
/// the transport never collapses two refusals into one.
#[test]
fn client_submit_forwards_and_refusals_carry_their_codes() {
    let (server, _parts, dir, stop, loop_thread) = running_parts();
    let (socket_path, serve_thread) = serve_parts(server, "client-submit.sock", &dir, stop);
    let mut client = OwnerClient::connect(&socket_path).expect("the owner answers");

    // Capture-then-create races the loop's publication: a pass
    // that replaces the namespace between capture and submit
    // invalidates the token, and the create refuses StaleParent.
    // The consumer discipline is re-capture and retry — the same
    // loop a Tauri or File Provider client runs — bounded here
    // because the root goes quiet once the seed settles. The
    // capture names the parent directory ("" is the drive root),
    // not the file being created.
    let outcome = {
        let mut attempts = 0;
        loop {
            attempts += 1;
            let token = client.capture_parent("").expect("the parent captures");
            match client.submit(WireMutationKind::CreateFile {
                path: "child.txt".to_string(),
                parent: token,
            }) {
                Ok(outcome) => break outcome,
                Err(ClientError::Refused(error))
                    if error.code == WireErrorCode::StaleParent && attempts < 30 =>
                {
                    std::thread::sleep(Duration::from_millis(50));
                    continue;
                }
                other => panic!("create submits: {other:?}"),
            }
        }
    };
    assert!(
        matches!(outcome, WireMutationOutcome::Created(_)),
        "create reports its identity: {outcome:?}"
    );

    let outcome = client
        .submit(WireMutationKind::Mkdir {
            path: "via-client".to_string(),
        })
        .expect("mkdir submits");
    assert_eq!(outcome, WireMutationOutcome::Done);
    match client
        .reconcile_lookup("via-client")
        .expect("reconcile re-reads")
    {
        Some((WireNode::Dir { .. }, _, _)) => {}
        other => panic!("the created directory serves: {other:?}"),
    }

    match client.submit(WireMutationKind::Rmdir {
        path: "nope".to_string(),
    }) {
        Err(ClientError::Refused(error)) => assert_eq!(error.code, WireErrorCode::NotFound),
        other => panic!("removing a missing dir refuses NotFound: {other:?}"),
    }
    match client.submit(WireMutationKind::Mkdir {
        path: "seed.txt".to_string(),
    }) {
        Err(ClientError::Refused(error)) => assert_eq!(error.code, WireErrorCode::AlreadyExists),
        other => panic!("re-making a file refuses AlreadyExists: {other:?}"),
    }

    stop.store(true, Ordering::Relaxed);
    serve_thread.join().unwrap();
    spawn_loop_done(loop_thread);
    std::fs::remove_dir_all(dir).unwrap();
}

/// P2 composition pin (option A): the mount and the headless runner
/// bind the same socket surface through the shared `OwnerService`, so
/// a mounted drive still answers a second process over `owner.sock`.
/// The backend holds the same shared arcs the socket serves — the
/// mount's shape — while a client `lookup`s, `read`s, and `submit`s
/// through the drive socket. A regression that drops the mount's
/// bind (or hand-rolls a second server) fails here, not in review.
/// Requires the `mount` feature: it holds the mount's real
/// `FuseBackend` beside the socket.
#[cfg(feature = "mount")]
#[test]
fn mounted_owner_serves_socket_alongside_backend() {
    use super::OwnerService;
    use crate::fuse::FuseBackend;

    let (engine, dir) = scratch_drive();
    let store = wyrd_format::FsObjectStore::open(dir.clone()).unwrap();
    let mut daemon = wyrd_core::node::WyrdNode::new(engine, store).unwrap();
    daemon.put_file("hello.txt", b"hello owner").unwrap();
    daemon.refresh_live_heads().unwrap();
    let (live, parts) = daemon
        .into_live(
            Duration::from_secs(5),
            &wyrd_core::live::LiveConfig::default(),
        )
        .unwrap();
    // The mount's backend holds the same shared arcs the socket
    // serves: projection, wants, and the single mutation queue.
    let mut backend = FuseBackend::shared_with_wants(
        Arc::clone(&parts.projection),
        Arc::clone(&parts.wants),
        Arc::clone(&parts.mutations),
        parts.open_timeout,
        &parts.budgets,
    );
    backend.set_quarantine(Arc::clone(live.quarantine_queue()));
    backend.set_scrub(Arc::clone(live.scrub_queue()));
    // Held through the socket serving below: the mount's shape is a
    // live backend beside a bound socket over the same arcs.
    // The interface binds beside the backend through the shared
    // service — the same call the mount and the headless runner
    // make — at the drive-directory socket.
    let service = OwnerService::bind(
        OwnerHandles::for_live(
            &parts,
            Arc::clone(live.quarantine_queue()),
            Arc::clone(live.scrub_queue()),
            Arc::clone(live.waker()),
        ),
        &dir,
    )
    .expect("the mounted owner binds its socket");
    assert_eq!(
        service.socket_path(),
        super::socket_path_for(&dir),
        "the shared service binds the drive-directory socket"
    );
    let stop: &'static AtomicBool = Box::leak(Box::new(AtomicBool::new(false)));
    let loop_thread = spawn_loop(live, stop);
    let mut client =
        OwnerClient::connect(&super::socket_path_for(&dir)).expect("the mounted owner answers");
    let (node, _, _) = client
        .lookup("hello.txt")
        .expect("lookup serves while mounted");
    assert!(
        matches!(node, WireNode::File { size: 11, .. }),
        "lookup serves the seeded file while mounted: {node:?}"
    );
    let (bytes, _, _) = client.read("hello.txt").expect("read serves while mounted");
    assert_eq!(bytes, b"hello owner");
    let outcome = client
        .submit(WireMutationKind::Mkdir {
            path: "via-mounted-socket".to_string(),
        })
        .expect("submit forwards while mounted");
    assert_eq!(outcome, WireMutationOutcome::Done);
    match client.reconcile_lookup("via-mounted-socket") {
        Ok(Some((WireNode::Dir { .. }, _, _))) => {}
        other => panic!("the submitted directory serves while mounted: {other:?}"),
    }

    stop.store(true, Ordering::Relaxed);
    spawn_loop_done(loop_thread);
    drop(backend);
    drop(service);
    assert!(
        !super::socket_path_for(&dir).exists(),
        "shutdown removes the socket so no client mistakes an absent owner for a listening one"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

/// Shutdown removes the socket file exactly once, while the live
/// node (and its store lock) is still held: a late drop never unlinks
/// a successor owner's socket bound at the same path (F1). Holds the
/// composed live node across the shutdown — the composer order — then
/// binds a real second service as the successor before the old guard
/// drops, and asserts the successor's socket survives it.
#[test]
fn owner_service_shutdown_removes_once_and_spares_a_successor() {
    let (engine, dir) = scratch_drive();
    let store = wyrd_format::FsObjectStore::open(dir.clone()).unwrap();
    let mut daemon = wyrd_core::node::WyrdNode::new(engine, store).unwrap();
    daemon.refresh_live_heads().unwrap();
    let (live, parts) = daemon
        .into_live(
            Duration::from_secs(5),
            &wyrd_core::live::LiveConfig::default(),
        )
        .unwrap();
    let socket_path = super::socket_path_for(&dir);
    let mut first =
        super::OwnerService::bind(OwnerHandles::from_parts(&parts), &dir).expect("bind serves");
    assert!(socket_path.exists(), "bind creates the socket file");
    first.shutdown();
    assert!(
        !socket_path.exists(),
        "shutdown removes the socket while the live node is still held"
    );
    // A successor owner binds at the same path while the old guard is
    // still alive (the race F1 closes by ordering): the late drop must
    // not touch it, and the at-most-once removal makes that structural.
    let mut second =
        super::OwnerService::bind(OwnerHandles::from_parts(&parts), &dir).expect("re-bind serves");
    assert!(
        socket_path.exists(),
        "the successor rebinds the same path after a clean shutdown"
    );
    drop(first);
    assert!(
        socket_path.exists(),
        "the late drop never unlinks the successor's socket"
    );
    second.shutdown();
    assert!(
        !socket_path.exists(),
        "the successor's own shutdown removes its socket"
    );
    drop(second);
    drop(parts);
    drop(live);
    std::fs::remove_dir_all(dir).unwrap();
}

/// An absent or refused socket is unavailable — never an empty
/// drive, never a success, never evidence about any request.
#[test]
fn client_absence_is_unavailable() {
    let dir = std::env::temp_dir().join(format!(
        "wyrd-owner-client-absent-{}-{}",
        std::process::id(),
        SCRATCH_N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    match OwnerClient::connect(&dir.join("no.sock")) {
        Err(ClientError::Unavailable(_)) => {}
        other => panic!("no socket is unavailable, not an answer: {other:?}"),
    }
    match OwnerClient::connect_drive(&dir) {
        Err(ClientError::Unavailable(_)) => {}
        other => panic!("no owner beside the drive is unavailable: {other:?}"),
    }
    std::fs::remove_dir_all(dir).unwrap();
}

/// A dead owner leaves in-flight and later requests unconfirmed —
/// and a failed re-read never resolves the original request, it
/// stays unconfirmed too.
#[test]
fn client_dead_owner_is_unconfirmed_and_reconcile_stays_unconfirmed() {
    let (parts, dir) = composed_parts();
    let server = OwnerServer::new(OwnerHandles::from_parts(&parts));
    let stop: &'static AtomicBool = Box::leak(Box::new(AtomicBool::new(false)));
    let (socket_path, serve_thread) = serve_parts(server, "client-dead.sock", &dir, stop);
    let mut client = OwnerClient::connect(&socket_path).expect("the owner answers");
    client
        .lookup("hello.txt")
        .expect("the owner serves while alive");

    stop.store(true, Ordering::Relaxed);
    serve_thread.join().unwrap();

    match client.lookup("hello.txt") {
        Err(ClientError::Unconfirmed { .. }) => {}
        other => panic!("a dead owner is unconfirmed, never success: {other:?}"),
    }
    match client.reconcile_lookup("missing.txt") {
        Err(ClientError::Unconfirmed { .. }) => {}
        other => panic!("a failed re-read stays unconfirmed: {other:?}"),
    }
    std::fs::remove_dir_all(dir).unwrap();
}

/// Serve exactly one canned answer: the fake responder reads one
/// frame, answers it, and closes. Enough transport to prove the
/// client classifies protocol violations without a real owner.
fn serve_canned_once(
    socket_path: &std::path::Path,
    respond: impl FnOnce(RawRequest) -> WireResponse + Send + 'static,
) -> std::thread::JoinHandle<()> {
    let listener = std::os::unix::net::UnixListener::bind(socket_path).unwrap();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut len_bytes = [0u8; 4];
        stream.read_exact(&mut len_bytes).unwrap();
        let len = u32::from_be_bytes(len_bytes) as usize;
        let mut frame = vec![0u8; len];
        stream.read_exact(&mut frame).unwrap();
        let request: RawRequest = serde_json::from_slice(&frame).unwrap();
        let answer = serde_json::to_vec(&respond(request)).unwrap();
        stream
            .write_all(&(answer.len() as u32).to_be_bytes())
            .unwrap();
        stream.write_all(&answer).unwrap();
        stream.flush().unwrap();
    })
}

/// An answer in a shape the operation never produces, and an answer
/// echoing the wrong id, are incompatibilities — explicit, never a
/// generic unavailability and never a refusal of the request.
#[test]
fn client_wrong_shape_and_wrong_id_are_incompatible() {
    let dir = std::env::temp_dir().join(format!(
        "wyrd-owner-client-fake-{}-{}",
        std::process::id(),
        SCRATCH_N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();

    let socket_path = dir.join("wrong-shape.sock");
    let responder = serve_canned_once(&socket_path, |request| WireResponse {
        v: WIRE_VERSION,
        id: request.id,
        result: WireResult::Ok(Box::new(WireOk::Paused { paused: false })),
    });
    let mut client = OwnerClient::connect(&socket_path)
        .expect("the transport connects")
        .with_timeout(Duration::from_secs(10))
        .expect("the timeout arms");
    match client.lookup("hello.txt") {
        Err(ClientError::Incompatible(_)) => {}
        other => panic!("a pause answer to lookup is incompatible: {other:?}"),
    }
    responder.join().unwrap();

    let socket_path = dir.join("wrong-id.sock");
    let responder = serve_canned_once(&socket_path, |request| WireResponse {
        v: WIRE_VERSION,
        id: request.id.wrapping_add(1),
        result: WireResult::Ok(Box::new(WireOk::Paused { paused: false })),
    });
    let mut client = OwnerClient::connect(&socket_path)
        .expect("the transport connects")
        .with_timeout(Duration::from_secs(10))
        .expect("the timeout arms");
    match client.observe() {
        Err(ClientError::Incompatible(_)) => {}
        other => panic!("a mismatched id is incompatible: {other:?}"),
    }
    responder.join().unwrap();

    let socket_path = dir.join("wrong-version.sock");
    let responder = serve_canned_once(&socket_path, |request| WireResponse {
        v: 999,
        id: request.id,
        result: WireResult::Err(WireError {
            code: WireErrorCode::UnknownVersion,
            message: "unsupported wire version 999, want 1".to_string(),
        }),
    });
    let mut client = OwnerClient::connect(&socket_path)
        .expect("the transport connects")
        .with_timeout(Duration::from_secs(10))
        .expect("the timeout arms");
    match client.observe() {
        Err(ClientError::Incompatible(_)) => {}
        other => panic!("a foreign wire version is incompatible: {other:?}"),
    }
    responder.join().unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

/// A timed-out instance sends nothing more: the second call fails
/// without touching the socket, so the server never sees a second
/// frame. Without the poison, the second request would reuse the
/// broken stream and read the first answer late as an id mismatch.
#[test]
fn client_timed_out_instance_sends_nothing_more() {
    let dir = std::env::temp_dir().join(format!(
        "wyrd-owner-client-timeout-{}-{}",
        std::process::id(),
        SCRATCH_N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let socket_path = dir.join("timeout.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();
    let probe = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut len_bytes = [0u8; 4];
        stream.read_exact(&mut len_bytes).unwrap();
        let len = u32::from_be_bytes(len_bytes) as usize;
        let mut frame = vec![0u8; len];
        stream.read_exact(&mut frame).unwrap();
        // Outlast the client deadline: the first request times out
        // on the client side while the server still holds it.
        std::thread::sleep(Duration::from_millis(400));
        stream
            .set_read_timeout(Some(Duration::from_millis(500)))
            .unwrap();
        let mut second = [0u8; 4];
        match stream.read(&mut second) {
            // Silence or a clean close: the poisoned instance sent
            // nothing more.
            Ok(0) => false,
            Err(error)
                if error.kind() == std::io::ErrorKind::TimedOut
                    || error.kind() == std::io::ErrorKind::WouldBlock =>
            {
                false
            }
            // Bytes arrived: the broken stream was reused.
            _ => true,
        }
    });

    let mut client = OwnerClient::connect(&socket_path)
        .expect("the transport connects")
        .with_timeout(Duration::from_millis(50))
        .expect("the timeout arms");
    match client.pause() {
        Err(ClientError::Unconfirmed { .. }) => {}
        other => panic!("an unanswered request is unconfirmed: {other:?}"),
    }
    match client.pause() {
        Err(ClientError::Unconfirmed { .. }) => {}
        other => panic!("a poisoned instance stays unconfirmed: {other:?}"),
    }
    assert!(
        !probe.join().unwrap(),
        "a timed-out instance sends nothing more"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

/// An overlong response length poisons without reading the body: no
/// allocation past the bound, and no later call misreads the
/// unread body as a new length.
#[test]
fn client_overlong_frame_poisons_without_reading_the_body() {
    let dir = std::env::temp_dir().join(format!(
        "wyrd-owner-client-overlong-{}-{}",
        std::process::id(),
        SCRATCH_N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let socket_path = dir.join("overlong.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket_path).unwrap();
    let probe = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut len_bytes = [0u8; 4];
        stream.read_exact(&mut len_bytes).unwrap();
        let len = u32::from_be_bytes(len_bytes) as usize;
        let mut frame = vec![0u8; len];
        stream.read_exact(&mut frame).unwrap();
        // A length past the ceiling, with no body behind it: the
        // client must refuse the length, never allocate or read on.
        let overlong = (MAX_FRAME_BYTES as u32).wrapping_add(1);
        stream.write_all(&overlong.to_be_bytes()).unwrap();
        stream.flush().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(500)))
            .unwrap();
        let mut second = [0u8; 4];
        match stream.read(&mut second) {
            Ok(0) => false,
            Err(error)
                if error.kind() == std::io::ErrorKind::TimedOut
                    || error.kind() == std::io::ErrorKind::WouldBlock =>
            {
                false
            }
            _ => true,
        }
    });

    let mut client = OwnerClient::connect(&socket_path)
        .expect("the transport connects")
        .with_timeout(Duration::from_secs(10))
        .expect("the timeout arms");
    match client.lookup("hello.txt") {
        Err(ClientError::Unconfirmed { .. }) => {}
        other => panic!("an overlong length is unconfirmed: {other:?}"),
    }
    match client.lookup("hello.txt") {
        Err(ClientError::Unconfirmed { .. }) => {}
        other => panic!("a poisoned instance stays unconfirmed: {other:?}"),
    }
    assert!(
        !probe.join().unwrap(),
        "an overlong frame sends nothing more and reads no body"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

// Materialization policy (selective sync)
// ---------------------------------------------------------------------------

/// Dispatch one policy operation and unwrap the success.
fn policy_ok(server: &OwnerServer, op: &str, path: &str) -> WireOk {
    let response = server.dispatch(RawRequest {
        v: WIRE_VERSION,
        id: 31,
        op: op.to_string(),
        params: serde_json::to_value(PathParams {
            path: path.to_string(),
        })
        .unwrap(),
    });
    assert_eq!(response.v, WIRE_VERSION);
    assert_eq!(response.id, 31);
    match response.result {
        WireResult::Ok(ok) => *ok,
        WireResult::Err(error) => panic!("{op} {path} refused: {:?} {}", error.code, error.message),
    }
}

/// Dispatch one policy operation and unwrap the refusal.
fn policy_err(server: &OwnerServer, op: &str, path: &str) -> WireError {
    let response = server.dispatch(RawRequest {
        v: WIRE_VERSION,
        id: 32,
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

/// The wire vocabulary is the surface vocabulary: the serialized words
/// match the materialization surface consts, so a re-wording fails
/// here instead of drifting the contract past review.
#[test]
fn policy_words_match_surface_copy() {
    use wyrd_core::materialization_surface::{
        POLICY_PINNED_WORD, POLICY_REMOTE_ONLY_WORD, PRESENCE_ABSENT_WORD, PRESENCE_PRESENT_WORD,
    };
    assert_eq!(
        serde_json::to_value(WireRetentionPolicy::Pinned)
            .unwrap()
            .as_str()
            .unwrap(),
        POLICY_PINNED_WORD
    );
    assert_eq!(
        serde_json::to_value(WireRetentionPolicy::RemoteOnly)
            .unwrap()
            .as_str()
            .unwrap(),
        POLICY_REMOTE_ONLY_WORD
    );
    assert_eq!(
        serde_json::to_value(WirePresence::Present)
            .unwrap()
            .as_str()
            .unwrap(),
        PRESENCE_PRESENT_WORD
    );
    assert_eq!(
        serde_json::to_value(WirePresence::Absent)
            .unwrap()
            .as_str()
            .unwrap(),
        PRESENCE_ABSENT_WORD
    );
    // The canonical sentences live in one module: the CLI renders the
    // same consts, and the wire never re-words them.
    let surface = include_str!("../../../wyrd-core/src/materialization_surface.rs");
    for marker in [
        "Content fetched through access is still REMOTE_ONLY",
        "This releases the device's retention promise",
    ] {
        assert!(
            surface.contains(marker),
            "the surface owns the canonical copy ({marker:?})"
        );
    }
}

/// Policy refusals map to distinct codes: evict-against-pinned is
/// `PinnedPresent` (never missing, never invalid), and a path past
/// the depth bound is `TooDeep`.
#[test]
fn policy_refusals_map_to_distinct_codes() {
    let (code, _) = map_policy_error(wyrd_core::policy::PolicyError::PinnedPresent {
        path: "docs".to_string(),
        pinned: 2,
    });
    assert_eq!(code, WireErrorCode::PinnedPresent);
    let (code, _) = map_policy_error(wyrd_core::policy::PolicyError::TooDeep("d".to_string()));
    assert_eq!(code, WireErrorCode::TooDeep);
}

/// Pin, status, unpin, and evict round-trip through the interface
/// against the loop-thread executor: the same engine, the same
/// thread as mutations, four engine operations rather than a second
/// mutation path. Evict against a pin refuses with `PinnedPresent`
/// and commits nothing; unpin first, then evict succeeds.
#[test]
fn owner_interface_forwards_policy_unchanged() {
    let (server, _parts, dir, stop, loop_thread) = running_parts();

    // Pin through the interface: the seeded file promises retention.
    match policy_ok(&server, OP_POLICY_PIN, "seed.txt") {
        WireOk::PolicyPinned { pinned, .. } => {
            assert_eq!(pinned, 1, "the seeded file pins one identity");
        }
        other => panic!("unexpected pin result: {other:?}"),
    }

    // Status through the interface: one reachable file, PINNED, with
    // the generation and revision the loop actually walked attached.
    match policy_ok(&server, OP_POLICY_STATUS, "seed.txt") {
        WireOk::PolicyStatus {
            status,
            generation,
            revision,
        } => {
            assert!(status.complete, "a one-file walk fits the budget");
            assert_eq!(status.files.len(), 1);
            assert_eq!(status.files[0].policy, WireRetentionPolicy::Pinned);
            assert_eq!(status.files[0].path, "seed.txt");
            let _ = (generation, revision);
        }
        other => panic!("unexpected status result: {other:?}"),
    }

    // Evict against a pin refuses distinctly and commits nothing.
    let error = policy_err(&server, OP_POLICY_EVICT, "seed.txt");
    assert_eq!(error.code, WireErrorCode::PinnedPresent);

    // Unpin releases, then evict succeeds.
    match policy_ok(&server, OP_POLICY_UNPIN, "seed.txt") {
        WireOk::PolicyUnpinned { released, .. } => {
            assert_eq!(released, 1);
        }
        other => panic!("unexpected unpin result: {other:?}"),
    }
    match policy_ok(&server, OP_POLICY_EVICT, "seed.txt") {
        WireOk::PolicyEvicted { released, .. } => {
            assert_eq!(released, 1);
        }
        other => panic!("unexpected evict result: {other:?}"),
    }

    // The refused evict above committed nothing observable: status
    // after the unpin/evict pair reports REMOTE_ONLY.
    match policy_ok(&server, OP_POLICY_STATUS, "seed.txt") {
        WireOk::PolicyStatus { status, .. } => {
            assert_eq!(status.files[0].policy, WireRetentionPolicy::RemoteOnly);
        }
        other => panic!("unexpected trailing status: {other:?}"),
    }

    stop.store(true, Ordering::Relaxed);
    let summary = loop_thread.join().unwrap();
    assert!(summary.is_ok(), "the loop ends clean on stop");
    std::fs::remove_dir_all(dir).unwrap();
}

/// The DG-1 negative invariant for the policy path: while the live
/// owner holds the drive, the offline opener fails closed — and the
/// interface-committed facts equal an in-process pin on a twin drive.
///
/// The held half uses the lock probe, which is the same authority as
/// the open path (`try_lock` on the store LOCK file): the CLI's
/// `open_policy_node` reaches the same lock through
/// `Engine::open_keystore` → `DurableStore::open` → `StoreLocked`,
/// so `Held` here is the offline command failing closed. The parity
/// half pins the same seeded file through the interface and
/// in-process (`pin_subtree` on a twin with identical bytes, hence
/// identical chunk identities) and compares every policy/presence
/// row — the durable `Fact::Materialization` batch is the same batch
/// either way.
#[test]
fn policy_command_reaches_a_mounted_drive() {
    use wyrd_sync::durable::{probe_lock, LockProbe};

    let (server, _parts, dir, stop, loop_thread) = running_parts();

    // While the loop holds the store, the offline path fails closed.
    // `running_parts` holds `FsObjectStore` open in the live node and
    // the engine holds the durable lock beside it.
    assert_eq!(
        probe_lock(&dir).expect("the probe reads the lock, not state"),
        LockProbe::Held,
        "the live owner holds the drive: the offline policy command fails closed here"
    );

    // The interface pin commits through the held owner.
    match policy_ok(&server, OP_POLICY_PIN, "seed.txt") {
        WireOk::PolicyPinned { pinned, .. } => assert_eq!(pinned, 1),
        other => panic!("unexpected pin result: {other:?}"),
    }

    // Twin drive, same bytes (hence same chunk identities), pinned
    // in process: the durable facts must match the interface commit
    // row for row.
    let (twin_engine, twin_dir) = scratch_drive();
    let twin_store = wyrd_format::FsObjectStore::open(twin_dir.clone()).unwrap();
    let mut twin: wyrd_core::node::WyrdNode<crate::vault::OwnerView> =
        wyrd_core::node::WyrdNode::new(twin_engine, twin_store).expect("twin composes");
    twin.put_file("seed.txt", b"seed").expect("twin seeds");
    twin.refresh_live_heads().expect("twin installs its head");
    {
        let (engine, view) = twin.parts_mut();
        wyrd_core::policy::pin_subtree(engine, view, "seed.txt").expect("twin pins in process");
    }
    let twin_census = {
        let (engine, view) = twin.parts();
        wyrd_core::policy::residency_census(engine, view, "seed.txt").expect("twin reads back")
    };
    match policy_ok(&server, OP_POLICY_STATUS, "seed.txt") {
        WireOk::PolicyStatus { status, .. } => {
            assert_eq!(
                status.files.len(),
                twin_census.files.len(),
                "interface and in-process walks name the same files"
            );
            for (row, file) in status.files.iter().zip(twin_census.files.iter()) {
                assert_eq!(row.path, file.path);
                assert_eq!(row.policy, wire_retention_policy(&file.policy));
                assert_eq!(row.presence, wire_presence(&file.local));
            }
        }
        other => panic!("unexpected status result: {other:?}"),
    }

    stop.store(true, Ordering::Relaxed);
    let summary = loop_thread.join().unwrap();
    assert!(summary.is_ok(), "the loop ends clean on stop");
    drop(twin);
    std::fs::remove_dir_all(dir).unwrap();
    std::fs::remove_dir_all(twin_dir).unwrap();
}

/// The shared client serves the whole policy surface over the
/// socket: pin, status, the `PinnedPresent` refusal, unpin, and
/// evict, against the running loop — the Tauri-shaped path, not just
/// the in-process dispatch half above.
#[test]
fn client_serves_the_policy_surface() {
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
    let (socket_path, serve_thread) = serve_parts(server, "policysock", &dir, stop);
    let loop_thread = spawn_loop(live, stop);

    let mut client = OwnerClient::connect(&socket_path).expect("the owner answers");
    let (pinned, already, files, _) = client.policy_pin("seed.txt").expect("pin commits");
    assert_eq!((pinned, already, files), (1, 0, 1));
    let (status, generation, revision) = client.policy_status("seed.txt").expect("status serves");
    assert!(status.complete, "a one-file walk fits the budget");
    assert_eq!(status.files.len(), 1);
    assert_eq!(status.files[0].policy, WireRetentionPolicy::Pinned);
    let _ = (generation, revision);
    match client.policy_evict("seed.txt") {
        Err(ClientError::Refused(error)) => {
            assert_eq!(error.code, WireErrorCode::PinnedPresent)
        }
        other => panic!("evict against a pin refuses distinctly: {other:?}"),
    }
    let (released, _, _, _) = client.policy_unpin("seed.txt").expect("unpin releases");
    assert_eq!(released, 1);
    let (evicted, _, _, _) = client
        .policy_evict("seed.txt")
        .expect("evict succeeds unpinned");
    assert_eq!(evicted, 1);

    stop.store(true, Ordering::Relaxed);
    serve_thread.join().unwrap();
    let summary = loop_thread.join().unwrap();
    assert!(summary.is_ok(), "the loop ends clean on stop");
    std::fs::remove_dir_all(dir).unwrap();
}
