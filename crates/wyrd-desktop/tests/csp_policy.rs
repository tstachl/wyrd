//! Content-Security-Policy pin: the desktop webview loads the local
//! bundle only (`apps/desktop/dist` through `frontendDist`), talks to
//! the backend over the loopback IPC channel, and loads nothing
//! remote — no CDN, no dev server, no frames, no plugins, no forms.
//!
//! The policy lives in `tauri.conf.json` (`app.security.csp`) as a
//! per-directive map. Tauri appends its own bootstrap nonces and
//! hashes at compile time, so `script-src` and `style-src` stay
//! `'self'`-only: the built output carries no inline scripts or
//! styles, and Svelte `style:` directives compile to CSSOM calls, not
//! style attributes. `data:` on `img-src` covers bundler-inlined
//! images only (an `<img>` payload never executes script); `asset:`
//! names Tauri's local-file protocol for future bundled-file views.
//!
//! Any widening — a remote host, `unsafe-inline`, `unsafe-eval`, a
//! wildcard, `blob:` — fails here first, before review.

use serde_json::{Map, Value};

/// The exact directives the shipped webview runs under. A new
/// directive (a first remote font host, a first frame) updates this
/// table and its reason together — never silently beside it.
fn expected_directives() -> Vec<(&'static str, Vec<&'static str>)> {
    vec![
        (
            "default-src",
            vec!["'self'", "ipc:", "http://ipc.localhost"],
        ),
        ("script-src", vec!["'self'"]),
        ("style-src", vec!["'self'"]),
        ("img-src", vec!["'self'", "asset:", "data:"]),
        ("font-src", vec!["'self'"]),
        (
            "connect-src",
            vec!["'self'", "ipc:", "http://ipc.localhost"],
        ),
        ("object-src", vec!["'none'"]),
        ("base-uri", vec!["'self'"]),
        ("form-action", vec!["'none'"]),
        ("frame-src", vec!["'none'"]),
        ("frame-ancestors", vec!["'none'"]),
    ]
}

/// The shipped `app.security.csp` map. Both tests read the same
/// value, so a drifted config fails both the same way.
fn shipped_csp() -> Map<String, Value> {
    let conf: Value = serde_json::from_str(include_str!("../tauri.conf.json"))
        .expect("tauri.conf.json must parse");
    conf.pointer("/app/security/csp")
        .expect("app.security.csp must exist")
        .as_object()
        .expect("app.security.csp must be a per-directive map, never null")
        .clone()
}

/// A source the loopback-only policy never carries: dynamic
/// execution, a remote or wildcard origin, or a local-file scheme
/// outside its single directive. The loopback IPC origin
/// `http://ipc.localhost` is the one `http:`-shaped exception —
/// everything else shaped like a scheme or a host is remote.
fn is_forbidden(directive: &str, source: &str) -> bool {
    match source {
        "'unsafe-inline'" | "'unsafe-eval'" | "'wasm-unsafe-eval'" | "'strict-dynamic'"
        | "blob:" => true,
        "data:" => directive != "img-src",
        "http://ipc.localhost" => false,
        _ => {
            source.starts_with("http:")
                || source.starts_with("https:")
                || source.starts_with("ws:")
                || source.starts_with("wss:")
                || source.contains("//")
                || source == "*"
                || source.starts_with("*.")
        }
    }
}

/// Every forbidden `(directive, source)` pair in a policy map. Empty
/// on the shipped config; non-empty on any widening the sweep
/// understands, including scheme-qualified hosts the exact-token
/// comparison would miss.
fn violations(map: &Map<String, Value>) -> Vec<(String, String)> {
    let mut found = Vec::new();
    for (directive, sources) in map {
        let text = sources
            .as_str()
            .unwrap_or_else(|| panic!("CSP {directive} must be a source string"));
        for source in text.split_whitespace() {
            if is_forbidden(directive, source) {
                found.push((directive.clone(), source.to_string()));
            }
        }
    }
    found
}

/// The policy is present and matches the pinned table exactly: no
/// missing directive, no extra directive, no extra source.
#[test]
fn csp_matches_the_pinned_local_only_table() {
    let map = shipped_csp();
    assert_eq!(
        map.len(),
        expected_directives().len(),
        "unexpected CSP directives: {map:?}"
    );
    for (directive, sources) in expected_directives() {
        let actual = map
            .get(directive)
            .unwrap_or_else(|| panic!("CSP is missing {directive}: {map:?}"));
        let actual_sources: Vec<&str> = actual
            .as_str()
            .unwrap_or_else(|| panic!("CSP {directive} must be a source string: {map:?}"))
            .split_whitespace()
            .collect();
        assert_eq!(
            actual_sources, sources,
            "CSP {directive} drifted: {actual_sources:?}"
        );
    }
}

/// Belt and braces: even if the table above is edited, no forbidden
/// source can appear anywhere in the shipped policy — and the sweep
/// itself is proven to fire on widened shapes.
#[test]
fn csp_never_allows_remote_or_dynamic_execution() {
    let map = shipped_csp();
    assert!(
        violations(&map).is_empty(),
        "shipped CSP carries a forbidden source: {:?}",
        violations(&map)
    );
    assert!(
        !map.contains_key("script-src-attr")
            && !map.contains_key("script-src-elem")
            && !map.contains_key("style-src-attr")
            && !map.contains_key("style-src-elem"),
        "granular overrides must not smuggle in what script-src/style-src refuse: {map:?}"
    );
}

/// The sweep fires on the widenings it claims to catch: a
/// scheme-qualified host, a host wildcard, a non-loopback scheme, and
/// `data:` outside `img-src`. If this test ever goes green on a
/// silent sweep, the backstop above is decorative.
#[test]
fn csp_sweep_fires_on_widened_shapes() {
    let widened: Map<String, Value> = serde_json::from_value(serde_json::json!({
        "default-src": "'self' ipc: http://ipc.localhost",
        "script-src": "'self' https://cdn.example.com",
        "style-src": "'self' 'unsafe-inline'",
        "img-src": "'self' *.evil.test",
        "font-src": "'self'",
        "connect-src": "http://ipc.localhost wss://evil.test/socket",
        "object-src": "'none'",
        "base-uri": "'self' data:",
        "form-action": "'none'",
        "frame-src": "'none'",
        "frame-ancestors": "'none'",
    }))
    .expect("the widened fixture must parse");
    let found = violations(&widened);
    for wanted in [
        ("script-src", "https://cdn.example.com"),
        ("style-src", "'unsafe-inline'"),
        ("img-src", "*.evil.test"),
        ("connect-src", "wss://evil.test/socket"),
        ("base-uri", "data:"),
    ] {
        assert!(
            found
                .iter()
                .any(|(directive, source)| directive == wanted.0 && source == wanted.1),
            "the sweep must catch {wanted:?}: {found:?}"
        );
    }
    // The loopback exception stays quiet: the IPC origin alone is
    // not a violation on any directive that pins it.
    let loopback_only: Map<String, Value> = serde_json::from_value(serde_json::json!({
        "connect-src": "'self' ipc: http://ipc.localhost",
    }))
    .expect("the loopback fixture must parse");
    assert!(
        violations(&loopback_only).is_empty(),
        "http://ipc.localhost is the loopback exception, never a violation"
    );
}
