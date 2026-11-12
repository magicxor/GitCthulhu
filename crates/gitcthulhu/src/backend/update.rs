//! Update-check and avatar backend calls (owned by X5 after G0b; spec 21 §2.10, §5.6). Service calls are behind
//! `cfg(feature = "services")`. The Gravatar URL itself is `widgets::avatar::gravatar_url`.

/// Fetches an image (avatar) through the forge HTTP stack (30 s timeout, 16 MiB cap). Blocking.
#[cfg(feature = "services")]
pub fn fetch_image(
    services: &super::services::ServicesHandle,
    url: &str,
) -> Result<Vec<u8>, String> {
    services
        .services()
        .forge
        .fetch_image(url)
        .map_err(|e| e.to_string())
}

/// The update found by the last check, if any (no network).
#[cfg(feature = "services")]
pub fn cached_update_version(services: &super::services::ServicesHandle) -> Option<String> {
    services
        .services()
        .update
        .cached()
        .map(|update| update.version.to_string())
}
