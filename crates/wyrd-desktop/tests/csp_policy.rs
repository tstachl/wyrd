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

use serde_json::Value;

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
        ("frame-ancestors", vec!["'none'"]),
    ]
}

/// The policy is present and matches the pinned table exactly: no
/// missing directive, no extra directive, no extra source.
#[test]
fn csp_matches_the_pinned_local_only_table() {
    let conf: Value = serde_json::from_str(include_str!("../tauri.conf.json"))
        .expect("tauri.conf.json must parse");
    let csp = conf
        .pointer("/app/security/csp")
        .expect("app.security.csp must exist");
    let map = csp
        .as_object()
        .expect("app.security.csp must be a per-directive map, never null");
    assert_eq!(
        map.len(),
        expected_directives().len(),
        "unexpected CSP directives: {csp}"
    );
    for (directive, sources) in expected_directives() {
        let actual = map
            .get(directive)
            .unwrap_or_else(|| panic!("CSP is missing {directive}: {csp}"));
        let actual_sources: Vec<&str> = actual
            .as_str()
            .unwrap_or_else(|| panic!("CSP {directive} must be a source string: {csp}"))
            .split_whitespace()
            .collect();
        assert_eq!(
            actual_sources, sources,
            "CSP {directive} drifted: {actual_sources:?}"
        );
    }
}

/// Belt and braces: even if the table above is edited, these tokens
/// can never appear anywhere in the policy. `data:` is allowed only
/// on `img-src` (bundler-inlined images never execute script); every
/// other directive refuses it with the table.
#[test]
fn csp_never_allows_remote_or_dynamic_execution() {
    let conf: Value = serde_json::from_str(include_str!("../tauri.conf.json"))
        .expect("tauri.conf.json must parse");
    let csp = conf
        .pointer("/app/security/csp")
        .expect("app.security.csp must exist");
    let map = csp.as_object().expect("CSP must be a map");
    for forbidden in [
        "unsafe-inline",
        "unsafe-eval",
        "wasm-unsafe-eval",
        "http:",
        "https:",
        "ws:",
        "wss:",
        "blob:",
        "*",
    ] {
        for (directive, sources) in map {
            let text = sources
                .as_str()
                .unwrap_or_else(|| panic!("CSP {directive} must be a source string"));
            assert!(
                !text.split_whitespace().any(|source| source == forbidden),
                "CSP {directive} must never contain {forbidden}: {text}"
            );
        }
    }
    assert!(
        !map.contains_key("script-src-attr")
            && !map.contains_key("script-src-elem")
            && !map.contains_key("style-src-attr")
            && !map.contains_key("style-src-elem"),
        "granular overrides must not smuggle in what script-src/style-src refuse: {csp}"
    );
}
