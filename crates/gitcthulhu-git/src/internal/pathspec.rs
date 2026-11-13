//! Literal pathspec helpers: every path handed to Git is a `:(literal)` pathspec, never a bare path, so glob
//! characters and magic prefixes (`:!x`, `[12].txt`, `*`) in file names keep their literal meaning.

use std::ffi::OsString;

use crate::RelaPath;

/// The `:(literal)<path>` pathspec for `path`, as a command-line argument.
pub(crate) fn literal_arg(path: &RelaPath) -> OsString {
    let mut arg = OsString::from(":(literal)");
    match gix::path::from_byte_slice(path.as_slice()) {
        Ok(path) => arg.push(path.as_os_str()),
        // Only possible on Windows: a name that is not UTF-8 cannot be passed on the command line anyway.
        Err(_) => arg.push(String::from_utf8_lossy(path.as_slice()).as_ref()),
    }
    arg
}

/// The NUL-terminated list of `:(literal)<path>` pathspecs for `--pathspec-from-file=- --pathspec-file-nul`.
/// Paths are written as raw bytes, so non-UTF-8 names survive.
pub(crate) fn literal_nul_list(paths: &[RelaPath]) -> Vec<u8> {
    const PREFIX: &[u8] = b":(literal)";
    let mut out = Vec::with_capacity(paths.iter().map(|p| PREFIX.len() + p.len() + 1).sum());
    for path in paths {
        out.extend_from_slice(PREFIX);
        out.extend_from_slice(path.as_slice());
        out.push(0);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nul_list_prefixes_and_terminates_every_path() {
        let list = literal_nul_list(&["a b.txt".into(), ":!x".into()]);
        assert_eq!(
            list, b":(literal)a b.txt\0:(literal):!x\0",
            "each path is literal and NUL-terminated"
        );
    }

    #[test]
    fn nul_list_of_nothing_is_empty() {
        assert!(literal_nul_list(&[]).is_empty(), "no paths, no bytes");
    }

    #[test]
    fn literal_arg_keeps_glob_characters() {
        assert_eq!(
            literal_arg(&"[12].txt".into()),
            OsString::from(":(literal)[12].txt")
        );
    }
}
