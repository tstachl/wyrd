//! Shared readers for the contract suite: the workspace root,
//! manifest tables, the comment/string lexer, the `.rs` file walk,
//! and scratch-dir test helpers. No contracts live here; every
//! sibling contract module builds on these.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// A normal `[dependencies]` edge set for one member: manifest key to
/// the raw value, so aliases resolve through the `package` field.
pub(crate) type DepSet = BTreeMap<String, toml::Value>;

/// Effective package name for one manifest entry: the `package` rename
/// wins; a `workspace = true` inheritance consults the workspace table
/// for the rename; otherwise the key is the name.
pub(crate) fn effective_name(
    key: &str,
    value: &toml::Value,
    workspace_deps: &toml::Table,
) -> String {
    if let Some(table) = value.as_table() {
        if let Some(package) = table.get("package").and_then(|p| p.as_str()) {
            return package.to_owned();
        }
        if table.get("workspace").and_then(|w| w.as_bool()) == Some(true) {
            if let Some(renamed) = workspace_deps
                .get(key)
                .and_then(|w| w.as_table())
                .and_then(|w| w.get("package"))
                .and_then(|p| p.as_str())
            {
                return renamed.to_owned();
            }
        }
    }
    key.to_owned()
}

/// Collect production `[dependencies]` entries from a member manifest.
/// Any table named exactly `dependencies` counts (covers future
/// `[target.*.dependencies]`); `dev-dependencies` never does.
pub(crate) fn production_deps(manifest: &toml::Table) -> DepSet {
    let mut deps = DepSet::new();
    fn walk(table: &toml::Table, deps: &mut DepSet) {
        for (key, value) in table {
            if key == "dependencies" {
                if let Some(table) = value.as_table() {
                    deps.extend(table.iter().map(|(k, v)| (k.clone(), v.clone())));
                }
            } else if let Some(inner) = value.as_table() {
                walk(inner, deps);
            }
        }
    }
    walk(manifest, &mut deps);
    deps
}

/// Collect `[dev-dependencies]` entries from a member manifest. Any
/// table named exactly `dev-dependencies` counts (covers future
/// `[target.*.dev-dependencies]`); production deps never do.
pub(crate) fn dev_deps(manifest: &toml::Table) -> DepSet {
    let mut deps = DepSet::new();
    fn walk(table: &toml::Table, deps: &mut DepSet) {
        for (key, value) in table {
            if key == "dev-dependencies" {
                if let Some(table) = value.as_table() {
                    deps.extend(table.iter().map(|(k, v)| (k.clone(), v.clone())));
                }
            } else if let Some(inner) = value.as_table() {
                walk(inner, deps);
            }
        }
    }
    walk(manifest, &mut deps);
    deps
}

pub(crate) fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..")
}

pub(crate) fn read_manifest(dir: &Path) -> toml::Table {
    let text = fs::read_to_string(dir.join("Cargo.toml")).expect("member manifest is readable");
    text.parse().expect("member manifest parses as TOML")
}

pub(crate) fn workspace_dep_table(root_manifest: &toml::Table) -> toml::Table {
    root_manifest
        .get("workspace")
        .and_then(|w| w.get("dependencies"))
        .and_then(|d| d.as_table())
        .cloned()
        .unwrap_or_default()
}

/// Strip comments and string/char literals from whole file text,
/// preserving newlines so per-line matching below stays meaningful.
/// Handles `//` anywhere outside literals, nestable `/* */` block
/// comments, `"..."` with escapes, `b"..."`, raw `r"..."`/`r#"..."#`
/// forms, and `'x'` char literals (a `'` skips to its line's closing
/// quote; lifetimes without one pass through untouched). A small lexer
/// for a convention check, not a grammar: pathological nesting of
/// quotes inside lifetimes is out of scope.
pub(crate) fn code_text(text: &str) -> String {
    let mut code = String::with_capacity(text.len());
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        // Line comment: `//` in normal state runs to (not past) `\n`.
        if c == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                code.push(' ');
                i += 1;
            }
            continue;
        }
        // Block comment, nestable per the Rust grammar.
        if c == '/' && chars.get(i + 1) == Some(&'*') {
            let mut depth = 0;
            while i < chars.len() {
                if chars[i] == '/' && chars.get(i + 1) == Some(&'*') {
                    depth += 1;
                    i += 1;
                } else if chars[i] == '*' && chars.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    i += 1;
                    if depth == 0 {
                        i += 1;
                        break;
                    }
                }
                if chars[i] == '\n' {
                    code.push('\n');
                } else {
                    code.push(' ');
                }
                i += 1;
            }
            continue;
        }
        // Raw string: `r"..."`, `r#"..."#`, ... (and `br` variants).
        if (c == 'r' || (c == 'b' && chars.get(i + 1) == Some(&'r'))) && {
            let mut j = if c == 'b' { i + 1 } else { i };
            j += 1;
            while chars.get(j) == Some(&'#') {
                j += 1;
            }
            chars.get(j) == Some(&'"')
        } {
            let mut hashes = 0;
            let mut j = if c == 'b' { i + 2 } else { i + 1 };
            while chars.get(j) == Some(&'#') {
                hashes += 1;
                j += 1;
            }
            j += 1; // opening quote
            loop {
                if j >= chars.len() {
                    i = j;
                    break;
                }
                if chars[j] == '"' {
                    let mut k = j + 1;
                    let mut seen = 0;
                    while seen < hashes && chars.get(k) == Some(&'#') {
                        seen += 1;
                        k += 1;
                    }
                    if seen == hashes {
                        j = k;
                        i = j;
                        break;
                    }
                }
                if chars[j] == '\n' {
                    code.push('\n');
                }
                j += 1;
            }
            continue;
        }
        // Ordinary or byte string.
        if c == '"' || (c == 'b' && chars.get(i + 1) == Some(&'"')) {
            i += if c == 'b' { 2 } else { 1 };
            while i < chars.len() && chars[i] != '"' && chars[i] != '\n' {
                if chars[i] == '\\' {
                    i += 1;
                }
                i += 1;
            }
            i += usize::from(i < chars.len() && chars[i] == '"');
            continue;
        }
        // Char literal: skip to the line's closing quote when there is
        // one; otherwise this is a lifetime tick, pass it through.
        if c == '\'' {
            let rest: String = chars[i + 1..].iter().collect();
            let closes = rest.find('\'').is_some_and(|p| !rest[..p].contains('\n'));
            if closes {
                i += 1;
                while i < chars.len() && chars[i] != '\'' {
                    if chars[i] == '\\' {
                        i += 1;
                    }
                    i += 1;
                }
                i += 1;
                continue;
            }
            code.push(c);
            i += 1;
            continue;
        }
        code.push(c);
        i += 1;
    }
    code
}

/// Collect every `.rs` file under `dir`, as paths relative to
/// `root`. Build output (`target/`) is never descended into. Fails
/// closed: unreadable entries are errors, never silent skips. An
/// empty collection is returned, not failed, here — an empty walk
/// proves nothing, so each rule names its own non-vacuity verdict.
pub(crate) fn collect_rs_files(dir: &Path, root: &Path) -> (Vec<(String, String)>, Vec<String>) {
    let mut files: Vec<(String, String)> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    fn walk(dir: &Path, root: &Path, files: &mut Vec<(String, String)>, errors: &mut Vec<String>) {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(err) => {
                errors.push(format!("cannot read directory `{}`: {err}", dir.display()));
                return;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    errors.push(format!("cannot list entry in `{}`: {err}", dir.display()));
                    continue;
                }
            };
            let path = entry.path();
            if path.is_dir() {
                // Never descend into build output.
                if path.file_name().is_some_and(|n| n == "target") {
                    continue;
                }
                walk(&path, root, files, errors);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let rel = path
                    .strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .into_owned();
                match fs::read_to_string(&path) {
                    Ok(text) => files.push((rel, text)),
                    Err(err) => {
                        errors.push(format!("cannot read file `{}`: {err}", path.display()));
                    }
                }
            }
        }
    }
    walk(dir, root, &mut files, &mut errors);
    files.sort();
    (files, errors)
}

/// The quoted strings a `name = [ ... ]` flake binding holds, in
/// order. Structural parsing for a structural guard: the test fails
/// closed on layout drift rather than silently reading nothing. The
/// list may sit on one line or span several; parsing stops at `]`.
pub(crate) fn flake_string_list<'a>(flake: &'a str, name: &str) -> Vec<&'a str> {
    let mut items = Vec::new();
    let mut inside = false;
    for line in flake.lines() {
        let trimmed = line.trim();
        if !inside {
            let rest = trimmed.strip_prefix(name).unwrap_or("");
            let rest = rest.trim_start().strip_prefix('=').unwrap_or("");
            if !rest.trim_start().starts_with('[') {
                continue;
            }
            inside = true;
        }
        let mut parts = trimmed.split('"');
        while let (Some(_), Some(quoted)) = (parts.next(), parts.next()) {
            if !quoted.is_empty() {
                items.push(quoted);
            }
        }
        if trimmed.contains(']') {
            break;
        }
    }
    items
}

/// The member's own `[features]` table for alias expansion below.
/// Absent means no aliases to expand through.
pub(crate) fn member_features(manifest: &toml::Table) -> toml::Table {
    manifest
        .get("features")
        .and_then(|f| f.as_table())
        .cloned()
        .unwrap_or_default()
}

/// Check the distribution build's cargo args from the flake text: the
/// release closure builds `-p wyrd-cli` with default features, so the
/// live (non-comment) flake text may contain neither
/// `insecure-fast-kdf` nor `--all-features` — in any string form,
/// single-line or multiline, under any variable name. Comment stripping
/// is string-aware (a `#` inside `"..."` or `''...''` never starts a
/// comment), so a commented-out `cargoExtraArgs` neither satisfies the
/// presence check nor trips the token check, and a token that only
/// reaches the build through string concatenation is out of scope for
/// this structural guard (name it plainly or not at all). Not a
/// substitute for the resolved-graph check in CI
/// (`cargo tree -e normal,features` in workflow.yml) — matching the
/// whole live text rather than trusting line layout, and failing
/// closed when the flag vanishes.
/// Strip Nix `#` comments, string-aware: `"..."` (with `\` escapes)
/// and `''...''` (with `'''` escape) pass through intact, so a `#`
/// inside a string never starts a comment. Newlines are preserved so
/// diagnostics below keep their line numbers. Enough grammar for a
/// convention check; interpolation nesting beyond balanced quotes is
/// out of scope.
pub(crate) fn strip_nix_comments(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        let rest = &bytes[i..];
        if rest.starts_with(b"\"") {
            // Double-quoted string: copy through, honoring escapes.
            out.push('"');
            i += 1;
            while i < bytes.len() {
                let c = bytes[i];
                out.push(c as char);
                i += 1;
                if c == b'\\' && i < bytes.len() {
                    out.push(bytes[i] as char);
                    i += 1;
                } else if c == b'"' {
                    break;
                }
            }
        } else if rest.starts_with(b"''") {
            // Indented string: copy through, honoring `'''`.
            out.push_str("''");
            i += 2;
            while i < bytes.len() {
                if bytes[i..].starts_with(b"'''") {
                    out.push_str("'''");
                    i += 3;
                } else if bytes[i..].starts_with(b"''") {
                    out.push_str("''");
                    i += 2;
                    break;
                } else {
                    out.push(bytes[i] as char);
                    i += 1;
                }
            }
        } else if bytes[i] == b'#' {
            // Comment: skip to (not through) the newline.
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else {
            out.push(bytes[i] as char);
            i += 1;
        }
    }
    out
}

/// Scratch `wyrd-core`-shaped root (`<root>/src/`) under the temp
/// dir. The caller owns cleanup; tests remove the tree before
/// asserting so a failure never leaks a permission-stripped dir.
pub(crate) fn scratch_core_dir() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::temp_dir().join(format!(
        "wyrd-nostr-scope-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(root.join("src")).expect("scratch src dir is creatable");
    root
}

pub(crate) fn empty_ws() -> toml::Table {
    toml::Table::new()
}

pub(crate) fn deps(names: &[&str]) -> DepSet {
    names
        .iter()
        .map(|n| (n.to_string(), toml::Value::Boolean(true)))
        .collect()
}

pub(crate) fn manifest_deps(manifest: &str) -> DepSet {
    production_deps(&manifest.parse().unwrap())
}
