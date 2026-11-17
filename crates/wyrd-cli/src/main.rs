#![warn(missing_docs)]
//! `wyrd` CLI binary: thin process entry over the `wyrd-cli` library.
//! Argument parsing and process orchestration live in the library's
//! [`wyrd_cli::command`]; this binary forwards argv, prints the
//! error, and folds it into an exit status.

fn main() {
    if let Err(error) = wyrd_cli::command(std::env::args().skip(1).collect()) {
        eprintln!("error: {error}");
        std::process::exit(error.exit_code());
    }
}
