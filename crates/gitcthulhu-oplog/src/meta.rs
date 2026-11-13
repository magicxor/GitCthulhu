//! Entry metadata (`meta.json`, spec 17 §2.4) and the byte-preserving JSON encoding of paths and names.

use crate::types::{
    EntryId, EntryResult, EntryRole, InProgress, NetworkMarker, OperationKind, Scope,
    UntrackedCapture,
};

/// The current format version of `meta.json` and of snapshot trees. Entries with a higher version are listed but
/// never restored.
pub(crate) const FORMAT_VERSION: u32 = 1;

/// The machine-readable metadata of one entry, stored as `meta.json` in the entry commit's tree.
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EntryMeta {
    /// [`FORMAT_VERSION`] of the writer.
    pub version: u32,
    /// The [`EntryId`]: first entry of a chain = unix-millis at creation, then +1.
    pub seq: u64,
    /// The role in the undo/redo bookkeeping.
    pub role: EntryRole,
    /// Unknown strings deserialize to [`OperationKind::Unknown`].
    pub kind: OperationKind,
    /// English UI string, e.g. `Commit "Fix login"`.
    pub title: String,
    pub detail: Option<String>,
    pub scope: Scope,
    pub started_at_ms: i64,
    pub finished_at_ms: i64,
    pub result: EntryResult,
    /// Full ref names (incl. `HEAD`) that differ between before and after.
    pub refs_changed: Vec<String>,
    pub before_in_progress: Option<InProgress>,
    pub after_in_progress: Option<InProgress>,
    /// `false` if the after-capture failed (§3.4).
    pub after_captured: bool,
    /// Markers only.
    pub network: Option<NetworkMarker>,
    /// As requested by the descriptor.
    pub untracked: UntrackedCapture,
    /// Canonical workdir of the worktree that wrote the entry (forward slashes).
    pub worktree_path: String,
    pub app_version: String,
}

impl EntryMeta {
    /// The stable id of this entry.
    pub(crate) fn id(&self) -> EntryId {
        EntryId(self.seq)
    }

    /// Parse `meta.json`.
    ///
    /// Metadata of a newer format version that this build cannot fully parse (a new role, scope, result or
    /// in-progress kind) must not make the whole chain unreadable: such an entry is still listed, as a barrier that
    /// is never restored (`version > FORMAT_VERSION` makes it non-restorable). Unparsable metadata of this or an
    /// older version is an error.
    pub(crate) fn from_json(data: &[u8]) -> Result<EntryMeta, serde_json::Error> {
        let strict_err = match serde_json::from_slice::<EntryMeta>(data) {
            Ok(meta) => return Ok(meta),
            Err(err) => err,
        };
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Newer {
            version: u32,
            seq: u64,
            #[serde(default)]
            kind: Option<serde_json::Value>,
            #[serde(default)]
            title: String,
            #[serde(default)]
            detail: Option<String>,
            #[serde(default)]
            started_at_ms: i64,
            #[serde(default)]
            finished_at_ms: i64,
            #[serde(default)]
            worktree_path: String,
            #[serde(default)]
            app_version: String,
        }
        let newer = match serde_json::from_slice::<Newer>(data) {
            Ok(newer) if newer.version > FORMAT_VERSION => newer,
            _ => return Err(strict_err),
        };
        Ok(EntryMeta {
            version: newer.version,
            seq: newer.seq,
            role: EntryRole::Barrier {
                reason: format!(
                    "written by a newer version of GitCthulhu (format {})",
                    newer.version
                ),
            },
            kind: newer
                .kind
                .and_then(|kind| serde_json::from_value(kind).ok())
                .unwrap_or(OperationKind::Unknown),
            title: newer.title,
            detail: newer.detail,
            scope: Scope::RefsOnly,
            started_at_ms: newer.started_at_ms,
            finished_at_ms: newer.finished_at_ms,
            result: EntryResult::Succeeded,
            refs_changed: Vec::new(),
            before_in_progress: None,
            after_in_progress: None,
            after_captured: false,
            network: None,
            untracked: UntrackedCapture::None,
            worktree_path: newer.worktree_path,
            app_version: newer.app_version,
        })
    }
}

/// Serde helpers that write byte strings as JSON strings when they are valid UTF-8 and as byte arrays otherwise, so
/// paths and ref names stay byte-exact while the common case stays readable. Reading accepts both forms (that is
/// what `bstr`'s own `Deserialize` does).
pub(crate) mod bytes {
    use bstr::{BString, ByteSlice};
    use serde::{Serialize, Serializer, ser::SerializeSeq};

    /// A byte string that serializes as described in the [module docs](self).
    #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Deserialize)]
    #[serde(transparent)]
    pub(crate) struct Bytes(pub BString);

    impl Serialize for Bytes {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            serialize(&self.0, serializer)
        }
    }

    /// Serialize one byte string.
    pub(crate) fn serialize<S: Serializer>(
        value: &BString,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value.to_str() {
            Ok(s) => serializer.serialize_str(s),
            Err(_) => serializer.collect_seq(value.iter()),
        }
    }

    /// Serialize a list of byte strings.
    pub(crate) fn serialize_vec<S: Serializer>(
        values: &[BString],
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let mut seq = serializer.serialize_seq(Some(values.len()))?;
        for value in values {
            seq.serialize_element(&Bytes(value.clone()))?;
        }
        seq.end()
    }
}
