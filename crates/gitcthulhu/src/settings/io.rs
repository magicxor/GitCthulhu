//! JSON persistence shared by `settings.json`, `state.json` and `repos.json` (spec 21 §7.1).
//!
//! Files are UTF-8, pretty-printed and written atomically (temp file + rename). A missing file means defaults; a
//! corrupt file is renamed to `<name>.bak` and defaults are used.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui_kit::{App, AsyncApp, Global};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Quiet period after the last change before a file is written.
pub const SAVE_DEBOUNCE: Duration = Duration::from_millis(500);

/// Outcome of [`load`].
#[derive(Debug)]
pub enum Loaded<T> {
    /// The file was read.
    Ok(T),
    /// There was no file.
    Missing(T),
    /// The file could not be parsed; it was renamed to the contained backup path and defaults are returned.
    Corrupt { value: T, backup: Option<PathBuf> },
}

impl<T> Loaded<T> {
    /// The loaded (or default) value.
    pub fn into_value(self) -> T {
        match self {
            Loaded::Ok(v) | Loaded::Missing(v) | Loaded::Corrupt { value: v, .. } => v,
        }
    }
}

/// Path of the backup a corrupt `path` is renamed to (`settings.json` becomes `settings.json.bak`).
pub fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".bak");
    path.with_file_name(name)
}

/// Reads `path`. Missing files and corrupt files yield `T::default()`; corrupt files are renamed to `*.bak` so the
/// user can recover them.
pub fn load<T: DeserializeOwned + Default>(path: &Path) -> Loaded<T> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Loaded::Missing(T::default()),
        Err(err) => {
            tracing::warn!("cannot read {}: {err}", path.display());
            return Loaded::Missing(T::default());
        }
    };
    match serde_json::from_slice(&bytes) {
        Ok(value) => Loaded::Ok(value),
        Err(err) => {
            tracing::warn!("{} is corrupt ({err}); moving it aside", path.display());
            let backup = backup_path(path);
            // A previous backup is overwritten: only the most recent corrupt file is interesting.
            let _ = std::fs::remove_file(&backup);
            let backup = match std::fs::rename(path, &backup) {
                Ok(()) => Some(backup),
                Err(err) => {
                    tracing::warn!("cannot rename {}: {err}", path.display());
                    None
                }
            };
            Loaded::Corrupt {
                value: T::default(),
                backup,
            }
        }
    }
}

/// Writes `value` to `path` atomically: serialise, write a temporary sibling, rename over the target.
pub fn write_atomic<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let json = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    // Unique per call: a debounced background save and the synchronous flush on quit may write the same file at
    // the same time, and must not share (and truncate) one temporary file.
    static NEXT_TMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let serial = NEXT_TMP.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut tmp_name = std::ffi::OsString::from(".");
    tmp_name.push(path.file_name().unwrap_or_default());
    tmp_name.push(format!(".{}-{serial}.tmp", std::process::id()));
    let tmp = dir.join(tmp_name);
    let written = (|| -> io::Result<()> {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(&json)?;
        file.sync_all()
    })();
    // `rename` replaces an existing target on every platform we support.
    let result = written.and_then(|()| std::fs::rename(&tmp, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Generation counters of the pending debounced saves, one per file.
#[derive(Default)]
struct PendingSaves(std::collections::HashMap<&'static str, u64>);

impl Global for PendingSaves {}

/// Writes `snapshot` to `path` after [`SAVE_DEBOUNCE`] unless a newer `schedule_save` for the same `name` arrived
/// meanwhile. The write itself happens on a background thread.
pub fn schedule_save<T: Serialize + Send + 'static>(
    cx: &mut App,
    name: &'static str,
    path: PathBuf,
    snapshot: T,
) {
    let generation = {
        let pending = cx.default_global::<PendingSaves>();
        let slot = pending.0.entry(name).or_insert(0);
        *slot += 1;
        *slot
    };
    cx.spawn(async move |cx: &mut AsyncApp| {
        cx.background_executor().timer(SAVE_DEBOUNCE).await;
        let current = cx.update(|cx| cx.default_global::<PendingSaves>().0.get(name).copied());
        if current != Some(generation) {
            return;
        }
        let write = cx
            .background_executor()
            .spawn(async move { write_atomic(&path, &snapshot).map_err(|e| (path, e)) });
        if let Err((path, err)) = write.await {
            tracing::error!("cannot save {}: {err}", path.display());
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use serde::Deserialize;

    use super::*;

    #[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
    #[serde(default)]
    struct Sample {
        a: u32,
        b: String,
    }

    #[test]
    fn write_then_load_roundtrips() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("nested").join("sample.json");
        let value = Sample {
            a: 7,
            b: "x".into(),
        };
        write_atomic(&path, &value).expect("write");
        match load::<Sample>(&path) {
            Loaded::Ok(read) => assert_eq!(read, value, "value survives the roundtrip"),
            other => panic!("expected Loaded::Ok, got {other:?}"),
        }
    }

    #[test]
    fn corrupt_file_is_renamed_to_bak() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("sample.json");
        std::fs::write(&path, b"{ not json").expect("write garbage");
        let Loaded::Corrupt { value, backup } = load::<Sample>(&path) else {
            panic!("a corrupt file must be reported as corrupt");
        };
        assert_eq!(value, Sample::default(), "defaults are used");
        let backup = backup.expect("backup path");
        assert_eq!(
            backup.file_name().and_then(|n| n.to_str()),
            Some("sample.json.bak"),
            "the corrupt file is renamed to *.bak"
        );
        assert!(backup.exists(), "the backup exists");
        assert!(!path.exists(), "the corrupt original is gone");
    }

    #[test]
    fn missing_file_yields_defaults() {
        let dir = tempfile::tempdir().expect("temp dir");
        let loaded = load::<Sample>(&dir.path().join("none.json"));
        assert!(
            matches!(loaded, Loaded::Missing(ref v) if *v == Sample::default()),
            "a missing file is not an error: {loaded:?}"
        );
    }

    #[test]
    fn unknown_keys_are_tolerated() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("sample.json");
        std::fs::write(&path, br#"{"a": 3, "future_key": true}"#).expect("write");
        assert_eq!(
            load::<Sample>(&path).into_value(),
            Sample {
                a: 3,
                b: String::new()
            },
            "unknown keys are ignored, missing keys default"
        );
    }
}
