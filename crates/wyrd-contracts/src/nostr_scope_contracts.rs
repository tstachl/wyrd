//! `nostr` use inside `wyrd-core` stays inside one subsystem
//! directory (the mailbox decision): manifests cannot scope a
//! dependency to a module, so this scans source, not manifests.

use crate::contract_source::*;
use std::collections::BTreeSet;
use std::path::Path;

/// Does one stripped source line use a `nostr*` crate.
fn line_uses_nostr(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with("use nostr")
        || trimmed.starts_with("extern crate nostr")
        || line.contains("nostr::")
}

/// Subsystem directories (top-level `src/` children) holding `nostr*`
/// use, from an explicit file list. Pure so tests can pin the scanner
/// without touching the filesystem.
fn nostr_holding_subsystems(files: &[(&str, &str)]) -> BTreeSet<String> {
    files
        .iter()
        .filter(|(_, text)| code_text(text).lines().any(line_uses_nostr))
        .filter_map(|(path, _)| {
            Path::new(path)
                .components()
                .nth(1)
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
        })
        .collect()
}

/// `nostr*` use inside `wyrd-core` must stay within a single top-level
/// `src/` subsystem directory (the mailbox decision: control-plane
/// framing lives with sync control, never ambient across the node).
/// The walk fails closed: unreadable directories and files are
/// violations naming the path (never silent skips), and a walk that
/// examines no `.rs` files at all fails too — an empty file set
/// proves nothing, so "no `nostr*` references" and "nothing examined"
/// are different verdicts.
pub(crate) fn check_core_nostr_scope(core_dir: &Path) -> Vec<String> {
    let src = core_dir.join("src");
    // Paths stay relative to the crate dir, so the subsystem is
    // always at component index 1, matching the test convention.
    let (files, errors) = collect_rs_files(&src, core_dir);
    if !errors.is_empty() {
        return errors;
    }
    if files.is_empty() {
        return vec![format!(
            "`nostr*` scope check examined no `.rs` files under `{}`: \
             an empty walk proves nothing, failing closed",
            src.display()
        )];
    }
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(p, t)| (p.as_str(), t.as_str()))
        .collect();
    let subsystems = nostr_holding_subsystems(&refs);
    if subsystems.len() > 1 {
        vec![format!(
            "`nostr*` use in `wyrd-core` spans subsystems {subsystems:?}: \
             keep it inside the mailbox subsystem"
        )]
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod policy_tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::fs;
    #[test]
    fn nostr_scope_pins_to_one_subsystem() {
        let single = [
            ("src/mailbox/mod.rs", "use nostr_sdk::prelude::Client;"),
            (
                "src/mailbox/seen_store.rs",
                "let id = nostr::event::EventId::new();",
            ),
            ("src/session.rs", "use std::collections::BTreeMap;"),
        ];
        assert_eq!(
            nostr_holding_subsystems(&single),
            BTreeSet::from(["mailbox".to_owned()])
        );

        let spread = [
            ("src/mailbox/mod.rs", "use nostr_sdk::prelude::Client;"),
            ("src/session/mod.rs", "extern crate nostr;"),
        ];
        assert_eq!(
            nostr_holding_subsystems(&spread),
            BTreeSet::from(["mailbox".to_owned(), "session".to_owned()])
        );
    }

    #[test]
    fn nostr_scope_ignores_comments_and_urls() {
        let files = [
            ("src/session/mod.rs", "// see nostr:: docs for the framing"),
            (
                "src/session/mod.rs",
                "let x = 1;// attached nostr:: comment",
            ),
            (
                "src/session/mod.rs",
                "/* block nostr:: mention\nspanning lines, /* nested */ done */",
            ),
            ("src/node/mod.rs", "let url = \"http://relay/nostr::x\";"),
            ("src/node/mod.rs", "let raw = r#\"nostr:: in raw\"#;"),
            ("src/node/mod.rs", "let tick = &'static str;"),
            ("src/node/mod.rs", "use std::io;"),
        ];
        assert!(nostr_holding_subsystems(&files).is_empty());
    }

    #[test]
    fn nostr_scope_still_detects_real_use_beside_noise() {
        let files = [
            (
                "src/mailbox/mod.rs",
                "/* nostr:: in a comment */\nuse nostr_sdk::prelude::Client;",
            ),
            (
                "src/mailbox/seen_store.rs",
                "let doc = \"nostr:: in a string\";\nlet id = nostr::event::EventId::new();",
            ),
        ];
        assert_eq!(
            nostr_holding_subsystems(&files),
            BTreeSet::from(["mailbox".to_owned()])
        );
    }

    #[test]
    fn nostr_scope_fails_when_source_tree_is_missing() {
        let missing =
            std::env::temp_dir().join(format!("wyrd-nostr-scope-missing-{}", std::process::id()));
        // A crashed earlier run must not leave the tree behind and
        // flip this to the empty-dir case.
        let _ = fs::remove_dir_all(&missing);
        let violations = check_core_nostr_scope(&missing);
        assert_eq!(violations.len(), 1);
        assert!(
            violations[0].contains(&*missing.join("src").to_string_lossy()),
            "failure must name the unreadable tree: {violations:?}"
        );
    }

    #[test]
    fn nostr_scope_fails_when_no_files_are_examined() {
        let root = scratch_core_dir();
        let violations = check_core_nostr_scope(&root);
        fs::remove_dir_all(&root).expect("scratch cleanup");
        assert_eq!(violations.len(), 1);
        assert!(
            violations[0].contains("no `.rs` files"),
            "an empty walk must fail closed: {violations:?}"
        );
    }

    /// Permission-stripping proves the nested-dir branch on ordinary
    /// runners; on elevated runners (root reads through `0o000`) the
    /// same test asserts the complementary truth instead — a fully
    /// readable fixture passes clean — so it is an assertion everywhere.
    #[cfg(unix)]
    #[test]
    fn nostr_scope_surfaces_an_unreadable_nested_dir() {
        use std::os::unix::fs::PermissionsExt;
        let root = scratch_core_dir();
        let mailbox = root.join("src").join("mailbox");
        fs::create_dir_all(&mailbox).expect("scratch mailbox dir");
        fs::write(mailbox.join("mod.rs"), "use nostr_sdk::prelude::Client;").expect("scratch file");
        let opaque = root.join("src").join("opaque");
        fs::create_dir_all(&opaque).expect("scratch opaque dir");
        fs::set_permissions(&opaque, fs::Permissions::from_mode(0o000)).expect("strip permissions");
        // No root-proof way to fail `read_dir` from the filesystem
        // exists, so an elevated runner reads through `0o000`. Rather
        // than print-and-return (nextest hides passing-test output, so
        // a note proves nothing), assert the complementary truth on
        // that path: everything is readable, so the fixture tree must
        // yield a clean scope verdict. That keeps the test an assertion
        // on every runner and doubles as happy-path coverage of the
        // fixture itself.
        if fs::read_dir(&opaque).is_ok() {
            let violations = check_core_nostr_scope(&root);
            fs::remove_dir_all(&root).expect("scratch cleanup");
            assert!(
                violations.is_empty(),
                "fully readable fixture must pass clean: {violations:?}"
            );
            return;
        }
        let violations = check_core_nostr_scope(&root);
        fs::set_permissions(&opaque, fs::Permissions::from_mode(0o755)).expect("restore");
        fs::remove_dir_all(&root).expect("scratch cleanup");
        assert_eq!(violations.len(), 1);
        assert!(
            violations[0].contains("opaque"),
            "nested read failure must surface: {violations:?}"
        );
    }

    /// A dangling symlink fails `read_to_string` on every runner,
    /// including elevated ones that read through `0o000`, so the
    /// unreadable-file proof is runner-independent. `is_dir` follows
    /// links (false for a dangling link) and the `rs` extension still
    /// selects it, landing in exactly the file-error branch.
    #[cfg(unix)]
    #[test]
    fn nostr_scope_surfaces_an_unreadable_file() {
        let root = scratch_core_dir();
        let mailbox = root.join("src").join("mailbox");
        fs::create_dir_all(&mailbox).expect("scratch dirs");
        let file = mailbox.join("mod.rs");
        std::os::unix::fs::symlink("nowhere.rs", &file).expect("dangling symlink");
        assert!(
            fs::read_to_string(&file).is_err(),
            "the dangling link must stay unreadable"
        );
        let violations = check_core_nostr_scope(&root);
        fs::remove_dir_all(&root).expect("scratch cleanup");
        assert_eq!(violations.len(), 1);
        assert!(
            violations[0].contains("mod.rs"),
            "unreadable file must surface: {violations:?}"
        );
    }
}
