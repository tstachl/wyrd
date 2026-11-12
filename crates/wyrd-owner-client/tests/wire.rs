//! Owned-protocol tests for `wyrd-owner-client`: the wire field
//! names stay pinned, legacy payloads parse, and the golden vectors
//! pin the bytes both sides agree on — with no domain crate in scope.
//! These moved with the types from the daemon's owner tests; the
//! vectors file moved unmodified.

use std::collections::BTreeSet;

use wyrd_owner_client::wire::*;

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
        identity: Some("ab".repeat(32)),
    })
    .unwrap();
    assert_eq!(keys(&node), set(&["File"]));
    assert_eq!(
        keys(&node["File"]),
        set(&["size", "executable", "chunks", "identity"])
    );

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
        paused: false,
    })
    .unwrap();
    assert_eq!(
        keys(&liveness),
        set(&["health", "report", "passes", "paused"])
    );
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

/// The entry-addressed mutations carry the required identity
/// precondition since v2; creations and folds do not (the parent
/// token, respectively the members, carry their own).
#[test]
fn mutation_kinds_carry_the_identity_precondition() {
    fn keys(value: &serde_json::Value) -> BTreeSet<String> {
        value
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<BTreeSet<_>>()
    }
    fn op_keys(kind: &WireMutationKind) -> BTreeSet<String> {
        let value = serde_json::to_value(&RawRequest {
            v: WIRE_VERSION,
            id: 7,
            op: OP_SUBMIT.to_string(),
            params: serde_json::to_value(SubmitParams { kind: kind.clone() }).unwrap(),
        })
        .unwrap();
        let kind_object = value["params"]["kind"]
            .as_object()
            .expect("one variant object");
        assert_eq!(kind_object.len(), 1, "externally tagged, one variant");
        keys(kind_object.values().next().unwrap())
    }
    for (name, body) in [
        (
            "CommitFile",
            WireMutationKind::CommitFile {
                path: "f".to_string(),
                base: WireFileIdentity {
                    size: 0,
                    executable: false,
                    chunks: vec![],
                },
                executable: false,
                content_hex: String::new(),
                expected_identity: "ab".repeat(32),
            },
        ),
        (
            "Unlink",
            WireMutationKind::Unlink {
                path: "f".to_string(),
                expected_identity: "ab".repeat(32),
            },
        ),
        (
            "Rename",
            WireMutationKind::Rename {
                from: "a".to_string(),
                to: "b".to_string(),
                no_replace: true,
                expected_identity: "ab".repeat(32),
            },
        ),
    ] {
        let shape = op_keys(&body);
        assert!(
            shape.contains("expected_identity"),
            "{name} carries the precondition"
        );
    }
    let mkdir = op_keys(&WireMutationKind::Mkdir {
        path: "d".to_string(),
    });
    assert!(
        !mkdir.contains("expected_identity"),
        "creations carry no entry precondition"
    );
}

#[test]
fn liveness_without_paused_parses_as_running() {
    let legacy = serde_json::json!({
        "health": {
            "stream_alive": false,
            "connected_relays": 0,
            "total_relays": 1,
            "saturation_recoveries": 0,
            "supervisor_ticks": 0,
            "relay_attached": false,
            "stream_recovery_attempts": 0,
            "relay_recovery_attempts": 0,
            "closed_subscriptions": 0
        },
        "report": null,
        "passes": 0
    });
    let liveness: WireLiveness = serde_json::from_value(legacy).unwrap();
    assert!(
        !liveness.paused,
        "a pre-pause liveness payload reads back running"
    );
}

#[test]
fn golden_vectors_pin_the_wire() {
    #[derive(Debug, serde::Deserialize)]
    struct Vector {
        name: String,
        json: serde_json::Value,
    }
    let vectors: Vec<Vector> =
        serde_json::from_str(include_str!("../vectors.json")).expect("vectors parse");
    assert!(!vectors.is_empty(), "the vectors pin at least one document");
    for vector in &vectors {
        let (kind, shape) = vector
            .name
            .split_once('/')
            .unwrap_or_else(|| panic!("vector names are kind/shape: {}", vector.name));
        match shape {
            "request" => {
                let request: RawRequest = serde_json::from_value(vector.json.clone())
                    .unwrap_or_else(|error| panic!("{} parses as a request: {error}", vector.name));
                assert_eq!(
                    request.v, WIRE_VERSION,
                    "{} carries the wire version",
                    vector.name
                );
                let expected_op = match kind {
                    "capture-parent" => OP_CAPTURE_PARENT,
                    "submit" => OP_SUBMIT,
                    "submit-create" => OP_SUBMIT,
                    "submit-commit" => OP_SUBMIT,
                    "lookup" => OP_LOOKUP,
                    "stat" => OP_STAT,
                    "readdir" => OP_READDIR,
                    "read" => OP_READ,
                    "observe" => OP_OBSERVE,
                    "liveness" => OP_LIVENESS,
                    "freshness" => OP_FRESHNESS,
                    "pause" => OP_PAUSE,
                    "resume" => OP_RESUME,
                    "policy-pin" => OP_POLICY_PIN,
                    "policy-unpin" => OP_POLICY_UNPIN,
                    "policy-evict" => OP_POLICY_EVICT,
                    "policy-status" => OP_POLICY_STATUS,
                    other => panic!("unknown vector operation: {other}"),
                };
                assert_eq!(
                    request.op, expected_op,
                    "{} names its operation",
                    vector.name
                );
                let round_trip = serde_json::to_value(&request).expect("requests re-serialize");
                assert_eq!(
                    round_trip, vector.json,
                    "{} re-serializes to the identical document",
                    vector.name
                );
            }
            "ok" => {
                let response: WireResponse = serde_json::from_value(vector.json.clone())
                    .unwrap_or_else(|error| {
                        panic!("{} parses as a response: {error}", vector.name)
                    });
                assert_eq!(response.v, WIRE_VERSION);
                match (kind, response.result) {
                    ("lookup", WireResult::Ok(ok)) => assert!(
                        matches!(*ok, WireOk::Lookup { .. }),
                        "the lookup sample answers a lookup"
                    ),
                    ("stat", WireResult::Ok(ok)) => match *ok {
                        WireOk::Stat {
                            ref attr,
                            generation,
                            revision,
                        } => {
                            assert_eq!(generation, 3);
                            assert_eq!(revision, 9);
                            assert!(
                                attr.identity.as_deref().map(str::len).unwrap_or(0) == 64,
                                "the stat sample carries a 256-bit identity as 64 hex chars"
                            );
                        }
                        ref other => panic!("the stat sample answers a stat: {other:?}"),
                    },
                    ("readdir", WireResult::Ok(ok)) => {
                        match *ok {
                            WireOk::Readdir {
                                ref entries,
                                generation,
                                revision,
                            } => {
                                assert_eq!(generation, 3);
                                assert_eq!(revision, 9);
                                assert_eq!(entries.len(), 6, "the readdir sample covers identity-bearing, legacy, merged-agreed, merged-disagreed, and conflict rows");
                                let kinds: Vec<&str> = entries
                                    .iter()
                                    .map(|entry| match entry.node {
                                        WireNode::File { .. } => "file",
                                        WireNode::Dir { .. } => "dir",
                                        WireNode::Symlink { .. } => "symlink",
                                        WireNode::MergedDir { .. } => "merged",
                                        WireNode::Conflict { .. } => "conflict",
                                    })
                                    .collect();
                                assert_eq!(
                                    kinds,
                                    vec!["file", "dir", "file", "merged", "merged", "conflict"]
                                );
                                assert!(
                                    entries.iter().any(|entry| match &entry.node {
                                        WireNode::File { identity, .. } => identity.is_none(),
                                        _ => false,
                                    }),
                                    "the readdir sample keeps a legacy entry without an identity key"
                                );
                                assert!(
                                    entries.iter().any(|entry| match &entry.node {
                                        WireNode::MergedDir { identity, .. } => identity.is_none(),
                                        _ => false,
                                    }),
                                    "the readdir sample keeps a merged-disagreed entry with a null identity"
                                );
                                assert!(
                                    entries.iter().any(|entry| matches!(entry.node, WireNode::Conflict { .. })),
                                    "the readdir sample keeps a conflict row the adapter must exclude"
                                );
                            }
                            ref other => panic!("the readdir sample answers a readdir: {other:?}"),
                        }
                    }
                    ("read", WireResult::Ok(ok)) => assert!(
                        matches!(*ok, WireOk::Read { .. }),
                        "the read sample answers a read"
                    ),
                    ("submit", WireResult::Ok(ok)) => assert!(
                        matches!(*ok, WireOk::Submitted { .. }),
                        "the submit sample answers a submit"
                    ),
                    ("policy-pin", WireResult::Ok(ok)) => assert!(
                        matches!(*ok, WireOk::PolicyPinned { .. }),
                        "the policy-pin sample answers a pin"
                    ),
                    ("policy-status", WireResult::Ok(ok)) => assert!(
                        matches!(
                            *ok,
                            WireOk::PolicyStatus {
                                status: WirePolicyStatus { complete: true, .. },
                                ..
                            }
                        ),
                        "the policy-status sample answers a complete status"
                    ),
                    other => panic!("unexpected ok sample: {other:?}"),
                }
            }
            "not-found" | "unknown-version" | "pinned-present" | "too-large" | "timeout"
            | "conflicted" | "unavailable" | "stale-entry" => {
                let response: WireResponse = serde_json::from_value(vector.json.clone())
                    .unwrap_or_else(|error| {
                        panic!("{} parses as a response: {error}", vector.name)
                    });
                let expected = match shape {
                    "not-found" => WireErrorCode::NotFound,
                    "pinned-present" => WireErrorCode::PinnedPresent,
                    "too-large" => WireErrorCode::TooLarge,
                    "timeout" => WireErrorCode::Timeout,
                    "conflicted" => WireErrorCode::Conflicted,
                    "unavailable" => WireErrorCode::Unavailable,
                    "stale-entry" => WireErrorCode::StaleEntry,
                    _ => WireErrorCode::UnknownVersion,
                };
                match response.result {
                    WireResult::Err(error) => assert_eq!(error.code, expected),
                    ok => panic!("{} is a refusal: {ok:?}", vector.name),
                }
            }
            other => panic!("unknown vector shape: {other}"),
        }
    }
    // One request through the framing, byte for byte: the
    // length-prefix plus the canonical body a second implementation
    // must reproduce exactly.
    let lookup = vectors
        .iter()
        .find(|vector| vector.name == "lookup/request")
        .unwrap();
    let request: RawRequest = serde_json::from_value(lookup.json.clone()).unwrap();
    let body = serde_json::to_vec(&request).unwrap();
    let mut framed = (body.len() as u32).to_be_bytes().to_vec();
    framed.extend_from_slice(&body);
    assert_eq!(
        framed,
        {
            let mut expected = (body.len() as u32).to_be_bytes().to_vec();
            expected.extend_from_slice(
                b"{\"v\":2,\"id\":7,\"op\":\"lookup\",\"params\":{\"path\":\"hello.txt\"}}",
            );
            expected
        },
        "the lookup request frames byte-for-byte"
    );
    // The submit-commit request carries the required identity
    // precondition: a v2 payload without it is malformed, never a
    // silent path-only mutation.
    let commit = vectors
        .iter()
        .find(|vector| vector.name == "submit-commit/request")
        .unwrap();
    let request: RawRequest = serde_json::from_value(commit.json.clone()).unwrap();
    let body = serde_json::to_vec(&request).unwrap();
    assert!(
        String::from_utf8_lossy(&body).contains("expected_identity"),
        "the commit sample carries the precondition"
    );
}

#[test]
fn policy_wire_field_names_are_pinned() {
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

    let row = serde_json::to_value(&WirePolicyRow {
        path: "a.txt".to_string(),
        policy: WireRetentionPolicy::Pinned,
        presence: WirePresence::Absent,
    })
    .unwrap();
    assert_eq!(keys(&row), set(&["path", "policy", "presence"]));

    let status_body = serde_json::to_value(&WirePolicyStatus {
        files: Vec::new(),
        conflicts: Vec::new(),
        dirs: 0,
        symlinks_skipped: 0,
        complete: true,
    })
    .unwrap();
    assert_eq!(
        keys(&status_body),
        set(&["files", "conflicts", "dirs", "symlinks_skipped", "complete"])
    );

    let status = serde_json::to_value(&WireOk::PolicyStatus {
        status: WirePolicyStatus {
            files: Vec::new(),
            conflicts: Vec::new(),
            dirs: 0,
            symlinks_skipped: 0,
            complete: true,
        },
        generation: 0,
        revision: 0,
    })
    .unwrap();
    assert_eq!(keys(&status), set(&["PolicyStatus",]));
    assert_eq!(
        keys(&status["PolicyStatus"]),
        set(&["status", "generation", "revision"])
    );

    let pinned = serde_json::to_value(&WireOk::PolicyPinned {
        pinned: 0,
        already_pinned: 0,
        files: 0,
        dirs: 0,
    })
    .unwrap();
    assert_eq!(
        keys(&pinned["PolicyPinned"]),
        set(&["pinned", "already_pinned", "files", "dirs"])
    );
}
