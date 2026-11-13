//! Crate-wide source rules (spec 21 header, §8.1 `test_import_rule`).

use core::prelude::v1::test;

use std::fs;
use std::path::{Path, PathBuf};

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("read source dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn sources() -> Vec<PathBuf> {
    let mut files = Vec::new();
    rust_files(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("src"),
        &mut files,
    );
    files
}

/// Only `src/lib.rs` may glob-import the kit: with `test-support` the glob re-exports GPUI's `test` attribute and
/// `#[gpui_kit::test]` recurses; and every `tests.rs` starts with `use core::prelude::v1::test;`.
#[test]
fn test_import_rule() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for file in sources() {
        let text = fs::read_to_string(&file).expect("read source");
        let relative = file.strip_prefix(&root).expect("inside src");
        if relative != Path::new("lib.rs") {
            assert!(
                !text.contains(concat!("use gpui_kit", "::*"))
                    && !text.contains(concat!("use gpui", "::*")),
                "{} must import gpui-kit items explicitly (glob imports are allowed in src/lib.rs only)",
                relative.display()
            );
        }
        if relative.file_name().is_some_and(|n| n == "tests.rs") && !text.trim().is_empty() {
            let first_code_line = text
                .lines()
                .find(|l| !l.trim().is_empty() && !l.trim_start().starts_with("//"))
                .unwrap_or_default();
            assert_eq!(
                first_code_line.trim(),
                "use core::prelude::v1::test;",
                "{} must start with `use core::prelude::v1::test;` before any other `use`",
                relative.display()
            );
        }
    }
}

/// No placeholders may remain in the files this package owns.
#[test]
fn no_todo_macros_in_sources() {
    for file in sources() {
        let text = fs::read_to_string(&file).expect("read source");
        if file.ends_with("tests.rs") {
            continue;
        }
        for needle in ["todo!(", "unimplemented!("] {
            assert!(
                !text.contains(needle),
                "{} contains `{needle}`",
                file.display()
            );
        }
    }
}
