// Embed the Tauri application context (`tauri.conf.json` plus
// `capabilities/`) at compile time. No codegen beyond the stock
// context: the IPC surface is hand-written in `src/commands.rs`.
//
// `generate_context!` requires the configured `frontendDist` to
// exist at compile time, but the frontend builds independently
// (`bun run build` in `apps/desktop/`, gitignored `dist/`): a lane
// that checks this crate without building the frontend first
// (plain `cargo check`, Rust CI) must not fail on a missing
// directory. Ensure the directory exists and let the missing
// *content* stay missing — an empty `dist/` compiles; a missing
// one does not.
fn main() {
    let manifest = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let frontend_dist = manifest.join("../../apps/desktop/dist");
    if let Err(error) = std::fs::create_dir_all(&frontend_dist) {
        panic!(
            "wyrd-desktop: cannot ensure frontend dist dir {}: {error}",
            frontend_dist.display()
        );
    }
    tauri_build::build()
}
