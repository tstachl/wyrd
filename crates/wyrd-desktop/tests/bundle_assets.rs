//! The macOS bundle must ship the app icon: the bundler ships
//! only the icons `tauri.conf.json` lists, so this pins the exact
//! `bundle.icon` set and the files it names. Regression: the key
//! was once absent and both the built and installed bundles shipped
//! iconless (no `Contents/Resources/icon.icns`, no
//! `CFBundleIconFile`).

/// The icon set the bundle ships, as listed in `tauri.conf.json`.
/// Any addition or removal here is a deliberate packaging decision
/// with a matching asset change, never silent drift.
const EXPECTED_ICONS: [&str; 5] = [
    "icons/32x32.png",
    "icons/128x128.png",
    "icons/128x128@2x.png",
    "icons/icon.icns",
    "icons/icon.ico",
];

/// The config, baked in at compile time like the sibling
/// `csp_policy.rs` pin: no working-directory dependence when the
/// test binary runs.
const TAURI_CONF: &str = include_str!("../tauri.conf.json");

#[test]
fn bundle_lists_the_icons_it_ships() {
    let config: serde_json::Value =
        serde_json::from_str(TAURI_CONF).expect("parse tauri.conf.json");
    let icons = config
        .pointer("/bundle/icon")
        .and_then(|icon| icon.as_array())
        .expect("bundle.icon lists the shipped icons");
    let listed: Vec<&str> = icons
        .iter()
        .map(|entry| entry.as_str().expect("icon entries are paths"))
        .collect();
    assert_eq!(
        listed, EXPECTED_ICONS,
        "bundle.icon is exactly the shipped set"
    );
    for path in EXPECTED_ICONS {
        assert!(
            std::path::Path::new(path).is_file(),
            "bundled icon exists: {path}"
        );
    }
}
