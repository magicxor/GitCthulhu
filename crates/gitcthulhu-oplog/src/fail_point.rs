//! Fail points for crash-recovery and failure-policy tests (cargo feature `test-hooks`).
//!
//! Without the feature, [`hit()`] compiles to nothing. With it, a test arms a fail point for one repository (keyed by
//! its git dir, so parallel tests don't interfere); the next time the oplog reaches that point in that repository,
//! the point fires once: it either returns an error or runs the test's action and continues.

/// Places in the oplog where a test can inject a failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
#[cfg_attr(
    not(feature = "test-hooks"),
    allow(
        dead_code,
        reason = "the restore points are reached by the restore engine (OP-WP3)"
    )
)]
pub enum FailPoint {
    /// At the start of every capture (`prepare`, `finish`, and the captures of a restore).
    Capture,
    /// In a restore, right after the worktree was written.
    AfterWorktree,
    /// In a restore, right before the ref transaction (use an action to move a ref and trip its guards).
    BeforeRefs,
    /// In a restore, right after the ref transaction.
    AfterRefs,
    /// In a restore, right after the index was written.
    AfterIndex,
    /// In a restore, make `git <op> --quit` fail.
    QuitFails,
}

#[cfg(feature = "test-hooks")]
mod imp {
    use std::{
        collections::HashMap,
        path::{Path, PathBuf},
    };

    use super::FailPoint;
    use crate::OplogError;

    enum Armed {
        Fail,
        Action(Box<dyn FnOnce() + Send>),
    }

    static ARMED: parking_lot::Mutex<Option<HashMap<(PathBuf, FailPoint), Armed>>> =
        parking_lot::Mutex::new(None);

    fn key(git_dir: &Path, point: FailPoint) -> (PathBuf, FailPoint) {
        let git_dir = gix::path::realpath(git_dir).unwrap_or_else(|_| git_dir.to_owned());
        (git_dir, point)
    }

    /// Make `point` fail once with an error in the repository whose git dir is `git_dir`.
    pub fn arm(git_dir: &Path, point: FailPoint) {
        ARMED
            .lock()
            .get_or_insert_with(HashMap::new)
            .insert(key(git_dir, point), Armed::Fail);
    }

    /// Run `action` once when `point` is reached in the repository whose git dir is `git_dir`, then continue.
    pub fn arm_action(git_dir: &Path, point: FailPoint, action: impl FnOnce() + Send + 'static) {
        ARMED
            .lock()
            .get_or_insert_with(HashMap::new)
            .insert(key(git_dir, point), Armed::Action(Box::new(action)));
    }

    /// Disarm every fail point of the repository whose git dir is `git_dir`.
    pub fn disarm_all(git_dir: &Path) {
        let git_dir = key(git_dir, FailPoint::Capture).0;
        if let Some(map) = ARMED.lock().as_mut() {
            map.retain(|(dir, _), _| *dir != git_dir);
        }
    }

    pub(crate) fn hit(repo: &gix::Repository, point: FailPoint) -> Result<(), OplogError> {
        let armed = {
            let mut guard = ARMED.lock();
            let Some(map) = guard.as_mut() else {
                return Ok(());
            };
            if map.is_empty() {
                return Ok(());
            }
            map.remove(&key(repo.git_dir(), point))
        };
        match armed {
            None => Ok(()),
            Some(Armed::Fail) => {
                let err = anyhow::anyhow!("fail point {point:?} triggered");
                Err(match point {
                    FailPoint::Capture => OplogError::Capture(err),
                    _ => OplogError::Other(err),
                })
            }
            Some(Armed::Action(action)) => {
                action();
                Ok(())
            }
        }
    }
}

#[cfg(feature = "test-hooks")]
pub use imp::{arm, arm_action, disarm_all};

/// Fire `point` if a test armed it for `repo` (feature `test-hooks`); otherwise do nothing.
#[cfg(feature = "test-hooks")]
pub(crate) fn hit(repo: &gix::Repository, point: FailPoint) -> Result<(), crate::OplogError> {
    imp::hit(repo, point)
}

/// Fire `point` if a test armed it for `repo` (feature `test-hooks`); otherwise do nothing.
#[cfg(not(feature = "test-hooks"))]
#[inline(always)]
pub(crate) fn hit(_repo: &gix::Repository, _point: FailPoint) -> Result<(), crate::OplogError> {
    Ok(())
}
