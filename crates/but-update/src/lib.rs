#![deny(missing_docs)]
//! Update checking for GitCthulhu against a static release manifest.
//!
//! There is no update server: a release publishes a JSON manifest (for example on GitHub Releases or
//! Pages) and the application compares its own version with it on the client.
//!
//! # Manifest
//!
//! ```json
//! {
//!   "version": "0.2.0",
//!   "pub_date": "2026-12-01T00:00:00Z",
//!   "notes": "markdown release notes",
//!   "release_url": "https://example.com/releases/v0.2.0",
//!   "platforms": {
//!     "windows-x86_64": { "url": "https://example.com/GitCthulhu_0.2.0_x64.msi", "signature": "<base64 minisig>" },
//!     "darwin-aarch64": { "url": "https://example.com/GitCthulhu_0.2.0_arm64.app.tar.gz", "signature": "..." }
//!   }
//! }
//! ```
//!
//! Platform keys are `<os>-<arch>` with the OS spelled `windows`, `linux` or `darwin` (Rust's `macos` is
//! mapped, see [`platform_key`]). Unknown fields are ignored.
//!
//! # Functionality
//!
//! - [`check_status`] / [`check_status_async`]: fetch the manifest, compare versions with `semver`, cache the
//!   outcome. A database lock held by another checker makes the call a no-op returning `Ok(None)`.
//! - [`fetch_status`] + [`record_status`]: the same without holding a database lock during the request.
//! - [`available_update`]: read the cached outcome without any network access; honours the snooze.
//! - [`suppress_update`]: snooze the notice for 1-720 hours.
//!
//! The cached outcome is only valid for the `cache_scope` it was produced for (the application passes its
//! version and release channel), so an upgrade or a channel switch never shows a stale notice.
//!
//! ```rust,no_run
//! use but_update::{CheckParams, available_update, check_status};
//!
//! # fn example(cache: &mut but_db::AppCacheHandle) -> Result<(), but_update::CheckError> {
//! let current = semver::Version::new(0, 1, 0);
//! let params = CheckParams {
//!     manifest_url: "https://example.com/latest.json",
//!     current_version: &current,
//!     cache_scope: "0.1.0/release",
//!     user_agent: "GitCthulhu/0.1.0",
//! };
//! check_status(&params, cache)?;
//! if let Some(update) = available_update(cache, &current, params.cache_scope)? {
//!     println!("version {} is available", update.available_version);
//! }
//! # Ok(())
//! # }
//! ```

/// Module with functionality around checking the app updateability status.
mod check;

pub use check::{
    CheckError, CheckParams, CheckUpdateStatus, Manifest, PlatformEntry, check_status,
    check_status_async, current_platform_key, fetch_status, platform_key, record_status,
};

/// Module with functionality for caching update check results.
pub mod cache;
pub use cache::{
    AvailableUpdate, available_update, available_update_at, last_checked, suppress_update,
};
