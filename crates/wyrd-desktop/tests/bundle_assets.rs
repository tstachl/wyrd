//! The macOS bundle must ship the app icon: the bundler ships
//! only the icons `tauri.conf.json` lists, so this pins the
//! `bundle.icon` array and the files it names. Regression: the key
//! was once absent and both the built and installed bundles shipped
//! iconless (no `Contents/Resources/icon.icns`, no
//! `CFBundleIconFile`).

#[test]
fn bundle_lists_the_icons_it_ships() {
    let manifest = std::fs::read_to_string("tauri.conf.json").expect("read tauri.conf.json");
    let config: serde_json::Value = serde_json::from_str(&manifest).expect("parse tauri.conf.json");
    let icons = config
        .pointer("/bundle/icon")
        .and_then(|icon| icon.as_array())
        .expect("bundle.icon lists the shipped icons");
    assert!(
        icons.iter().any(|entry| entry
            .as_str()
            .is_some_and(|path| path.ends_with("icon.icns"))),
        "an .icns is listed for the macOS bundle: {icons:?}"
    );
    for entry in icons {
        let path = entry.as_str().expect("icon entries are paths");
        assert!(
            std::path::Path::new(path).is_file(),
            "bundled icon exists: {path}"
        );
    }
}
