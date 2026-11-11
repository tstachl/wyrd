//! Drive-owner interface closure: the interface exposes no store
//! internals, is not a second engine, and its read side never
//! constructs a mailbox.

use crate::contract_source::*;

/// The drive-owner interface's production surface, for the three
/// source checks below: every `.rs` file under the interface
/// module, tests included (the tests must not construct a mailbox
/// either — the fake implements the trait, it never connects one).
fn owner_module_files() -> Vec<(String, String)> {
    let root = workspace_root();
    let dir = root.join("crates/wyrd-daemon/src/owner");
    let (files, errors) = collect_rs_files(&dir, &root);
    assert!(
        errors.is_empty(),
        "owner module source walk errors:\n- {}",
        errors.join("\n- ")
    );
    assert!(!files.is_empty(), "the owner module holds sources to check");
    files
}

/// The interface exposes no store internals (OD-16-C's closure
/// rule): no operation accepts or returns a `DurableStore`, a
/// `RuntimeState`, an `Engine`, or a `wyrd_sync::runtime` type. The
/// check is textual over the module — the forbidden names cannot
/// appear in signatures, bodies, or imports — and the server's
/// construction from shared handles (`OwnerHandles::from_parts`)
/// pins the shape by compilation.
#[test]
fn interface_exposes_no_store_internals() {
    const FORBIDDEN: &[&str] = &["DurableStore", "RuntimeState", "Engine<"];
    let files = owner_module_files();
    let borrowed: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    let mut violations = Vec::new();
    for (path, text) in &borrowed {
        let code = code_text(text);
        for (index, line) in code.lines().enumerate() {
            for marker in FORBIDDEN {
                if line.contains(marker) {
                    violations.push(format!(
                        "{path}:{} names store internals (`{marker}`): the interface serves shared \
                         projections and loop-published slots, never the store or the engine",
                        index + 1
                    ));
                }
            }
        }
    }
    // `wyrd_sync::runtime` types ride the same rule on the
    // operation surface (the server, its handles, and its auth):
    // no operation there names the module. The wire mapping
    // (`mapping.rs` — the conversions moved out of the old `wire.rs`
    // with the extraction, the protocol types now living in
    // `wyrd-owner-client`) is exempt by design — mapping a value requires
    // naming its source type, and the conversion functions are Rust-side
    // conveniences, invisible on the wire. The tests are
    // exempt because a test composer needs an engine to build a
    // drive worth serving.
    for (path, text) in &borrowed {
        let file = path.rsplit('/').next().unwrap_or(path);
        if file == "mapping.rs" || file == "tests_owner.rs" {
            continue;
        }
        let code = code_text(text);
        for (index, line) in code.lines().enumerate() {
            if line.contains("sync::runtime::") || line.contains("sync::runtime,") {
                violations.push(format!(
                    "{path}:{} names `wyrd_sync::runtime` (`{}`): the interface closure stays free \
                     of it",
                    index + 1,
                    line.trim()
                ));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "store-internal leaks:\n- {}",
        violations.join("\n- ")
    );
}

/// The interface is not a second engine: no call site for snapshot
/// authoring, carry, admission, or announcement anywhere in the
/// module. Forwarding a submission to the single queue is the
/// interface's job (child 2); interpreting it is not, and these
/// four names are the interpretation vocabulary. The shipped
/// surface carries this gate; the test file is exempt by name —
/// fixtures compose drives through the engine API exactly like any
/// composer does, and never implement engine semantics.
#[test]
fn the_interface_is_not_a_second_engine() {
    const FORBIDDEN: &[&str] = &[
        "author_snapshot",
        "carry_pending",
        "admit_device",
        "announce_snapshot",
    ];
    let files = owner_module_files();
    let shipped: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .filter(|(path, _)| !path.ends_with("tests_owner.rs"))
        .collect();
    assert!(
        !shipped.is_empty(),
        "the shipped surface holds sources to check"
    );
    let mut violations = Vec::new();
    for (path, text) in shipped {
        let code = code_text(text);
        for (index, line) in code.lines().enumerate() {
            for marker in FORBIDDEN {
                if line.contains(marker) {
                    violations.push(format!(
                        "{path}:{} names the engine vocabulary (`{marker}`): the interface forwards \
                         to the queue, it never implements the engine",
                        index + 1
                    ));
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "second-engine leaks:\n- {}",
        violations.join("\n- ")
    );
}

/// The read side never constructs a mailbox: no live mailbox type,
/// no seen store, no health sampling, no trait use beyond the test
/// fake's implementation. The behavioral half is
/// `interface_read_side_never_constructs_a_mailbox` in the daemon;
/// this test is the source check that makes the status contract's
/// rule (`status.rs:108-114`) a gate rather than a note.
#[test]
fn interface_read_side_never_constructs_a_mailbox() {
    const FORBIDDEN: &[&str] = &[
        "LiveMailbox",
        "SeenStore",
        "health_snapshot",
        "Mailbox::",
        ".send(",
        ".recv(",
        ".settle(",
        ".connect(",
    ];
    let files = owner_module_files();
    let borrowed: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    let mut violations = Vec::new();
    for (path, text) in borrowed {
        let code = code_text(text);
        for (index, line) in code.lines().enumerate() {
            for marker in FORBIDDEN {
                if line.contains(marker) {
                    violations.push(format!(
                        "{path}:{} reaches the mailbox (`{marker}`): the read side forwards the \
                         loop's observations, it never touches a mailbox",
                        index + 1
                    ));
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "mailbox construction leaks:\n- {}",
        violations.join("\n- ")
    );
}
