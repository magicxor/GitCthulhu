//! Entry points of the helper programs Git launches on our behalf: the askpass helper and the rebase editor.
//!
//! The GUI package ships them as its own binaries (`gitcthulhu-askpass`, `gitcthulhu-editor`); this crate has test
//! copies. Both binaries are one-liners calling into this module.

mod askpass;
mod editor;

use std::process::ExitCode;

/// Run the askpass helper: connect to the application's askpass server, forward the prompt and print the answer.
pub fn askpass_main() -> ExitCode {
    askpass::run()
}

/// Run the editor helper that Git starts as `GIT_SEQUENCE_EDITOR` / `GIT_EDITOR` during an interactive rebase.
pub fn editor_main() -> ExitCode {
    editor::run()
}
