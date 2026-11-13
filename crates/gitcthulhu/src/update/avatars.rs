//! The on-disk cache of downloaded avatars and the fetch policy around it (spec 21 §2.10).
//!
//! `widgets::avatar::AvatarCache` remembers images for the session only. This layer keeps them between sessions so a
//! repository with a few hundred authors does not hit Gravatar on every start, and so avatars still show offline:
//!
//! - an image is fresh for [`IMAGE_TTL`]; after that it is fetched again, but when the fetch *fails* (offline, server
//!   error) the stale image is used rather than falling back to the initial letter,
//! - "no such image" answers (Gravatar's 404) are remembered for [`ABSENT_TTL`] so addresses without a picture are
//!   not asked again and again,
//! - other failures are never cached: the next start tries again.
//!
//! Files are named after the SHA-256 of the URL (the URL of a Gravatar already carries only the hash of the e-mail
//! address, never the address itself) and written through a temporary file, so a crash never leaves a torn image.

use std::collections::VecDeque;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use futures::channel::oneshot;
use sha2::{Digest as _, Sha256};

use crate::backend::update::ImageFetch;

/// How long a cached image is used without asking the server again.
pub const IMAGE_TTL: Duration = Duration::from_secs(7 * 24 * 3600);
/// How long "this address has no picture" is remembered.
pub const ABSENT_TTL: Duration = Duration::from_secs(24 * 3600);
/// The cache keeps at most this many files (images and absence markers); older ones are removed by [`AvatarDiskCache::prune`].
pub const MAX_FILES: usize = 2000;

const IMAGE_EXT: &str = "img";
const ABSENT_EXT: &str = "none";

/// What the cache knows about a URL.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Lookup {
    /// A fresh image.
    Image(Vec<u8>),
    /// A fresh "no picture for this address".
    Absent,
    /// Nothing usable: ask the server.
    Miss,
}

/// A directory of cached avatars.
#[derive(Clone, Debug)]
pub struct AvatarDiskCache {
    dir: PathBuf,
}

impl AvatarDiskCache {
    /// A cache in `dir` (created on the first write).
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn path(&self, url: &str, ext: &str) -> PathBuf {
        let digest = Sha256::digest(url.as_bytes());
        let name: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        self.dir.join(format!("{name}.{ext}"))
    }

    /// The fresh entry for `url` at time `now`.
    pub fn lookup(&self, url: &str, now: SystemTime) -> Lookup {
        if let Some(bytes) = read_fresh(&self.path(url, IMAGE_EXT), IMAGE_TTL, now) {
            return Lookup::Image(bytes);
        }
        if read_fresh(&self.path(url, ABSENT_EXT), ABSENT_TTL, now).is_some() {
            return Lookup::Absent;
        }
        Lookup::Miss
    }

    /// The cached image of `url` however old it is (the fallback when the server cannot be reached).
    pub fn stale_image(&self, url: &str) -> Option<Vec<u8>> {
        fs::read(self.path(url, IMAGE_EXT)).ok()
    }

    /// Remembers `bytes` as the image of `url`; a failed write is logged and otherwise ignored (a cache is optional).
    pub fn store_image(&self, url: &str, bytes: &[u8]) {
        if let Err(err) = self.write(&self.path(url, IMAGE_EXT), bytes) {
            tracing::debug!("cannot cache an avatar: {err}");
            return;
        }
        let _ = fs::remove_file(self.path(url, ABSENT_EXT));
    }

    /// Remembers that `url` has no image.
    pub fn store_absent(&self, url: &str) {
        if let Err(err) = self.write(&self.path(url, ABSENT_EXT), b"") {
            tracing::debug!("cannot cache a missing avatar: {err}");
            return;
        }
        let _ = fs::remove_file(self.path(url, IMAGE_EXT));
    }

    fn write(&self, target: &Path, bytes: &[u8]) -> std::io::Result<()> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        fs::create_dir_all(&self.dir)?;
        let tmp = self.dir.join(format!(
            ".{}-{}.tmp",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let written = fs::File::create(&tmp).and_then(|mut file| file.write_all(bytes));
        match written.and_then(|()| fs::rename(&tmp, target)) {
            Ok(()) => Ok(()),
            Err(err) => {
                let _ = fs::remove_file(&tmp);
                Err(err)
            }
        }
    }

    /// Deletes the oldest files beyond `max_files`. Returns how many were removed.
    pub fn prune(&self, max_files: usize) -> usize {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return 0;
        };
        let mut files: Vec<(SystemTime, PathBuf)> = entries
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|ext| ext == IMAGE_EXT || ext == ABSENT_EXT)
            })
            .filter_map(|entry| {
                let modified = entry.metadata().ok()?.modified().ok()?;
                Some((modified, entry.path()))
            })
            .collect();
        if files.len() <= max_files {
            return 0;
        }
        files.sort_by_key(|(modified, _)| *modified);
        let excess = files.len() - max_files;
        files
            .into_iter()
            .take(excess)
            .filter(|(_, path)| fs::remove_file(path).is_ok())
            .count()
    }
}

/// The bytes of `path` when it exists and is younger than `ttl` at `now`.
fn read_fresh(path: &Path, ttl: Duration, now: SystemTime) -> Option<Vec<u8>> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    // A file from the future (clock change) counts as fresh: it is at most as old as zero.
    let age = now.duration_since(modified).unwrap_or(Duration::ZERO);
    (age < ttl).then(|| fs::read(path).ok()).flatten()
}

/// At most this many avatar downloads run at once. Each one blocks a background thread for up to the 30 s timeout
/// of the HTTP stack, so a log full of new authors on a black-holing network must not starve the git reads.
pub const PARALLEL: usize = 4;

/// Admits at most [`PARALLEL`] downloads at once; the others wait in arrival order (UI-thread state).
#[derive(Debug)]
pub struct DownloadGate {
    limit: usize,
    running: usize,
    waiting: VecDeque<oneshot::Sender<()>>,
}

impl Default for DownloadGate {
    fn default() -> Self {
        Self::new(PARALLEL)
    }
}

impl DownloadGate {
    /// A gate admitting `limit` downloads at once (at least one).
    pub fn new(limit: usize) -> Self {
        Self {
            limit: limit.max(1),
            running: 0,
            waiting: VecDeque::new(),
        }
    }

    /// Asks for a place: `None` means "go ahead now", `Some(turn)` resolves when a running download leaves.
    /// Every admitted download must call [`DownloadGate::leave`] exactly once.
    pub fn enter(&mut self) -> Option<oneshot::Receiver<()>> {
        if self.running < self.limit {
            self.running += 1;
            return None;
        }
        let (tx, rx) = oneshot::channel();
        self.waiting.push_back(tx);
        Some(rx)
    }

    /// A download finished: its place goes to the next waiter that is still there.
    pub fn leave(&mut self) {
        while let Some(next) = self.waiting.pop_front() {
            if next.send(()).is_ok() {
                return;
            }
        }
        self.running = self.running.saturating_sub(1);
    }

    /// Downloads admitted and not finished.
    pub fn running(&self) -> usize {
        self.running
    }

    /// Downloads waiting for a place.
    pub fn waiting(&self) -> usize {
        self.waiting.len()
    }
}

/// Why there is no image for an avatar (the caller shows the initial letter).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AvatarMiss {
    /// The address has no picture (remembered on disk for [`ABSENT_TTL`]).
    Absent,
    /// The server could not be reached or failed, and nothing is cached: worth asking again later.
    Unreachable(String),
}

impl std::fmt::Display for AvatarMiss {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AvatarMiss::Absent => f.write_str("this address has no avatar"),
            AvatarMiss::Unreachable(reason) => f.write_str(reason),
        }
    }
}

/// The avatar bytes for `url`: from the cache when fresh, else from `network`, else (network failure) from the stale
/// cache.
pub fn load(
    cache: &AvatarDiskCache,
    url: &str,
    now: SystemTime,
    network: impl FnOnce(&str) -> ImageFetch,
) -> Result<Vec<u8>, AvatarMiss> {
    match cache.lookup(url, now) {
        Lookup::Image(bytes) => return Ok(bytes),
        Lookup::Absent => return Err(AvatarMiss::Absent),
        Lookup::Miss => {}
    }
    match network(url) {
        ImageFetch::Found(bytes) => {
            cache.store_image(url, &bytes);
            Ok(bytes)
        }
        ImageFetch::NotFound => {
            cache.store_absent(url);
            Err(AvatarMiss::Absent)
        }
        ImageFetch::Failed(reason) => cache
            .stale_image(url)
            .ok_or(AvatarMiss::Unreachable(reason)),
    }
}
