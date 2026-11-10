// Embed the Tauri application context (`tauri.conf.json` plus
// `capabilities/`) at compile time. No codegen beyond the stock
// context: the IPC surface is hand-written in `src/commands.rs`.
fn main() {
    tauri_build::build()
}
