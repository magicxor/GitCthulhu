//! JSON persistence shared by `settings.json`, `state.json` and `repos.json` (spec 21 §7.1).
//!
//! Files are UTF-8, pretty-printed and written atomically (temp file + rename). A missing file means defaults; a
//! file that is no JSON object is renamed to a backup and defaults are used; a key whose value does not fit (wrong
//! type, out of range, an enum variant of a newer build) falls back to its default alone, the original file kept as
//! a backup.
//!
//! Several app instances may run at once: a save writes only the keys this process changed since it last read or
//! wrote the file, over the file's current content, and a process that changed nothing writes nothing on quit.

use std::collections::HashMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui_kit::{App, AsyncApp, Global};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

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
    /// The file was read, but the values of the `dropped` keys did not fit and their defaults are used; the
    /// original file was copied to `backup`. `rejected` holds the values as the file has them (see
    /// [`note_rejected`]).
    Partial {
        value: T,
        dropped: Vec<String>,
        rejected: serde_json::Map<String, Value>,
        backup: Option<PathBuf>,
    },
}

impl<T> Loaded<T> {
    /// The loaded (or default) value.
    pub fn into_value(self) -> T {
        match self {
            Loaded::Ok(v)
            | Loaded::Missing(v)
            | Loaded::Corrupt { value: v, .. }
            | Loaded::Partial { value: v, .. } => v,
        }
    }

    /// The value with `f` applied, keeping how it was loaded.
    pub fn map(self, f: impl FnOnce(T) -> T) -> Self {
        match self {
            Loaded::Ok(v) => Loaded::Ok(f(v)),
            Loaded::Missing(v) => Loaded::Missing(f(v)),
            Loaded::Corrupt { value, backup } => Loaded::Corrupt {
                value: f(value),
                backup,
            },
            Loaded::Partial {
                value,
                dropped,
                rejected,
                backup,
            } => Loaded::Partial {
                value: f(value),
                dropped,
                rejected,
                backup,
            },
        }
    }

    /// The values of the keys that did not fit, as the file has them (empty unless `Partial`).
    pub fn rejected(&self) -> serde_json::Map<String, Value> {
        match self {
            Loaded::Partial { rejected, .. } => rejected.clone(),
            _ => serde_json::Map::new(),
        }
    }

    /// What the user is told about loading `file` (`None` when it loaded as it was).
    pub fn notice(&self, file: &str) -> Option<String> {
        let kept = |backup: &Option<PathBuf>| match backup.as_deref().and_then(Path::file_name) {
            Some(name) => format!("; the original is kept as {}", name.to_string_lossy()),
            None => String::new(),
        };
        match self {
            Loaded::Ok(_) | Loaded::Missing(_) => None,
            Loaded::Corrupt { backup, .. } => Some(format!(
                "{file} could not be read and was reset{}",
                kept(backup)
            )),
            Loaded::Partial {
                dropped, backup, ..
            } => Some(format!(
                "{file}: reset {}{}",
                dropped.join(", "),
                kept(backup)
            )),
        }
    }
}

/// Path of the backup of `path` (`settings.json` becomes `settings.json.bak`).
pub fn backup_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".bak");
    path.with_file_name(name)
}

/// A backup path of `path` that does not exist yet: `<name>.bak`, else `<name>.<n>.bak`, so an earlier backup is
/// never replaced.
fn free_backup_path(path: &Path) -> PathBuf {
    let first = backup_path(path);
    if !first.exists() {
        return first;
    }
    let name = path.file_name().unwrap_or_default().to_os_string();
    (1u32..10_000)
        .map(|n| {
            let mut numbered = name.clone();
            numbered.push(format!(".{n}.bak"));
            path.with_file_name(numbered)
        })
        .find(|candidate| !candidate.exists())
        .unwrap_or(first)
}

/// Reads `path`. A missing file yields `T::default()`. A file that is no JSON object is renamed to a backup and
/// yields `T::default()`. A key whose value does not deserialise keeps its default while the other keys are read;
/// the original file is then copied to a backup so the user can recover the value.
pub fn load<T: DeserializeOwned + Serialize + Default>(path: &Path) -> Loaded<T> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Loaded::Missing(T::default()),
        Err(err) => {
            tracing::warn!("cannot read {}: {err}", path.display());
            return Loaded::Missing(T::default());
        }
    };
    let file = match serde_json::from_slice::<Value>(&bytes) {
        Ok(Value::Object(file)) => file,
        Ok(_) => return corrupt(path, "not a JSON object"),
        Err(err) => return corrupt(path, &err.to_string()),
    };
    if let Ok(value) = serde_json::from_value::<T>(Value::Object(file.clone())) {
        return Loaded::Ok(value);
    }
    // Key by key over the defaults: what fits is kept, what does not falls back to its default.
    let mut merged = match serde_json::to_value(T::default()) {
        Ok(Value::Object(defaults)) => defaults,
        _ => serde_json::Map::new(),
    };
    let mut dropped = Vec::new();
    let mut rejected = serde_json::Map::new();
    for (key, value) in file {
        let previous = merged.insert(key.clone(), value);
        if serde_json::from_value::<T>(Value::Object(merged.clone())).is_err() {
            let bad = match previous {
                Some(previous) => merged.insert(key.clone(), previous),
                None => merged.remove(&key),
            };
            if let Some(bad) = bad {
                rejected.insert(key.clone(), bad);
            }
            dropped.push(key);
        }
    }
    let Ok(value) = serde_json::from_value::<T>(Value::Object(merged)) else {
        return corrupt(path, "no value fits");
    };
    tracing::warn!("{}: reset {}", path.display(), dropped.join(", "));
    let backup = free_backup_path(path);
    let backup = match std::fs::copy(path, &backup) {
        Ok(_) => Some(backup),
        Err(err) => {
            tracing::warn!("cannot back up {}: {err}", path.display());
            None
        }
    };
    Loaded::Partial {
        value,
        dropped,
        rejected,
        backup,
    }
}

/// Moves the unreadable `path` aside and yields the defaults.
fn corrupt<T: Default>(path: &Path, why: &str) -> Loaded<T> {
    tracing::warn!("{} is corrupt ({why}); moving it aside", path.display());
    let backup = free_backup_path(path);
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
struct PendingSaves(HashMap<&'static str, u64>);

impl Global for PendingSaves {}

/// Per file, the value this process last read from or wrote to it: a save writes only the keys that differ from it.
#[derive(Default)]
struct Bases(HashMap<&'static str, Value>);

impl Global for Bases {}

/// Records `value` as what this process read from the file `name` (at startup).
pub fn set_base<T: Serialize>(cx: &mut App, name: &'static str, value: &T) {
    if let Ok(value) = serde_json::to_value(value) {
        cx.default_global::<Bases>().0.insert(name, value);
    }
}

/// Records that the file `name` holds the `rejected` values (a [`Loaded::Partial`] load used defaults instead), so
/// the next save, or the flush on quit, writes the defaults over them: the file then loads cleanly, rather than
/// warning and making one more backup at every start.
pub fn note_rejected(cx: &mut App, name: &'static str, rejected: serde_json::Map<String, Value>) {
    if rejected.is_empty() {
        return;
    }
    if let Some(Value::Object(base)) = cx.default_global::<Bases>().0.get_mut(name) {
        base.extend(rejected);
    }
}

/// `on_disk` with the keys in which `current` differs from `base` taken from `current` (and the keys `current`
/// dropped since `base` removed). Anything but three objects yields `current`.
pub(crate) fn merged(on_disk: Value, base: &Value, current: Value) -> Value {
    match (on_disk, base, current) {
        (Value::Object(mut disk), Value::Object(base), Value::Object(current)) => {
            for key in base.keys().filter(|key| !current.contains_key(*key)) {
                disk.remove(key);
            }
            for (key, value) in current {
                if base.get(&key) != Some(&value) {
                    disk.insert(key, value);
                }
            }
            Value::Object(disk)
        }
        (_, _, current) => current,
    }
}

/// Writes `current` to `path`; with a `base`, only the keys changed since then, over what the file holds now (an
/// unreadable file is replaced).
fn write_merged(path: &Path, base: Option<&Value>, current: Value) -> io::Result<()> {
    let value = match base {
        Some(base) => match std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        {
            Some(on_disk) => merged(on_disk, base, current),
            None => current,
        },
        None => current,
    };
    write_atomic(path, &value)
}

/// Writes `snapshot` to `path` after [`SAVE_DEBOUNCE`] unless a newer `schedule_save` for the same `name` arrived
/// meanwhile. Only the keys changed since this process last read or wrote the file are written, over its current
/// content (another instance may have changed others). The write itself happens on a background thread.
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
        let current = match serde_json::to_value(&snapshot) {
            Ok(current) => current,
            Err(err) => {
                tracing::error!("cannot save {}: {err}", path.display());
                return;
            }
        };
        let base = cx.update(|cx| {
            if cx.default_global::<PendingSaves>().0.get(name).copied() != Some(generation) {
                return None;
            }
            let bases = &mut cx.default_global::<Bases>().0;
            Some(bases.insert(name, current.clone()))
        });
        let Some(base) = base else {
            return;
        };
        let write = cx.background_executor().spawn(async move {
            write_merged(&path, base.as_ref(), current).map_err(|e| (path, e))
        });
        if let Err((path, err)) = write.await {
            tracing::error!("cannot save {}: {err}", path.display());
        }
    })
    .detach();
}

/// Writes `value` to `path` now (on quit), but only if this process changed it since it last read or wrote the
/// file, and then only the changed keys over the file's current content.
pub fn flush_now<T: Serialize>(cx: &App, name: &'static str, path: &Path, value: &T) {
    let current = match serde_json::to_value(value) {
        Ok(current) => current,
        Err(err) => {
            tracing::error!("cannot save {}: {err}", path.display());
            return;
        }
    };
    let base = cx.try_global::<Bases>().and_then(|b| b.0.get(name));
    if base == Some(&current) {
        return;
    }
    if let Err(err) = write_merged(path, base, current) {
        tracing::error!("cannot save {}: {err}", path.display());
    }
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
    fn an_earlier_backup_is_never_replaced() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("sample.json");
        std::fs::write(backup_path(&path), b"older").expect("an earlier backup");
        std::fs::write(&path, b"[1, 2]").expect("write a non-object");
        let Loaded::Corrupt { backup, .. } = load::<Sample>(&path) else {
            panic!("a file that is no object is corrupt");
        };
        assert_eq!(
            backup
                .as_deref()
                .and_then(Path::file_name)
                .and_then(|n| n.to_str()),
            Some("sample.json.1.bak"),
            "the new backup gets a free name"
        );
        assert_eq!(
            std::fs::read(backup_path(&path)).expect("still there"),
            b"older",
            "the earlier backup is kept"
        );
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

    #[test]
    fn a_value_that_does_not_fit_resets_only_its_key() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("sample.json");
        std::fs::write(&path, br#"{"a": -1, "b": "kept"}"#).expect("write");
        let Loaded::Partial {
            value,
            dropped,
            rejected,
            backup,
        } = load::<Sample>(&path)
        else {
            panic!("a wrong value is a partial load");
        };
        assert_eq!(
            Value::Object(rejected),
            serde_json::json!({"a": -1}),
            "the rejected value as the file has it"
        );
        assert_eq!(
            value,
            Sample {
                a: 0,
                b: "kept".into()
            },
            "only the key whose value does not fit falls back to its default"
        );
        assert_eq!(dropped, ["a"], "the reset key is named");
        let backup = backup.expect("a backup");
        assert_eq!(
            std::fs::read(&backup).expect("backup"),
            br#"{"a": -1, "b": "kept"}"#,
            "the original is kept"
        );
        assert!(path.exists(), "the file itself stays until the next save");
    }

    #[test]
    fn a_save_writes_only_the_changed_keys_over_the_file() {
        let base = serde_json::json!({"a": 1, "b": "x", "c": [1]});
        let on_disk = serde_json::json!({"a": 1, "b": "changed elsewhere", "c": [1], "d": true});
        let current = serde_json::json!({"a": 2, "b": "x", "c": [1]});
        assert_eq!(
            merged(on_disk, &base, current),
            serde_json::json!({"a": 2, "b": "changed elsewhere", "c": [1], "d": true}),
            "this process changed a; another changed b and added d: both survive"
        );
        let base = serde_json::json!({"/r1": {}, "/r2": {}});
        let on_disk = serde_json::json!({"/r1": {}, "/r2": {}, "/r3": {}});
        let current = serde_json::json!({"/r1": {}});
        assert_eq!(
            merged(on_disk, &base, current),
            serde_json::json!({"/r1": {}, "/r3": {}}),
            "a key this process removed goes, one added elsewhere stays"
        );
    }
}
