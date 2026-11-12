//! Error conversions shared by the modules that read with gix.

use std::any::Any;

use crate::GitError;

/// A gix error as [`GitError::Gix`], so callers can inspect its causes (`downcast_any_ref`, classification) instead of
/// matching on text. An error that already is a `gix::Error` is not wrapped a second time.
pub(crate) fn gix_error<E>(err: E) -> GitError
where
    E: std::error::Error + Send + Sync + 'static,
{
    let boxed: Box<dyn Any> = Box::new(err);
    match boxed.downcast::<gix::Error>() {
        Ok(already) => GitError::Gix(*already),
        Err(boxed) => match boxed.downcast::<E>() {
            Ok(err) => GitError::Gix(gix::Error::from_error(*err)),
            Err(_) => GitError::Other(anyhow::anyhow!(
                "an error of gix with an unexpected type was reported"
            )),
        },
    }
}
