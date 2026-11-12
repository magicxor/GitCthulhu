//! Image downloads (avatars, pictures in pull request descriptions): a timeout, a byte cap, and a signed-in
//! retry for GitHub's private images. Port of the old GUI's `backend/images.rs` (without the decoding).

use std::time::Duration;

use anyhow::{Context as _, Result, ensure};

/// The most an image is read to; GitHub takes uploads up to 10 MB.
const MAX_BYTES: usize = 16 << 20;

/// How long one download may take.
const TIMEOUT: Duration = Duration::from_secs(30);

/// Whether `url` is on GitHub itself and served over HTTPS: the only place an account's token is ever sent to.
pub(super) fn on_github(url: &reqwest::Url) -> bool {
    let github = |host: &str| host == "github.com" || host.ends_with(".githubusercontent.com");
    url.scheme() == "https" && url.host_str().is_some_and(github)
}

/// Parse `url`, which must be an absolute http(s) URL.
pub(super) fn parse(url: &str) -> Result<reqwest::Url> {
    let parsed = reqwest::Url::parse(url.trim()).context("The image URL is not valid")?;
    ensure!(
        matches!(parsed.scheme(), "http" | "https"),
        "Only http and https images can be downloaded"
    );
    Ok(parsed)
}

/// What `url` serves, given [`TIMEOUT`] and up to [`MAX_BYTES`]. With `token`, as a bearer token.
///
/// A failure status is an error carrying a [`but_github::HttpStatusError`], so it classifies like the
/// forge calls do. A redirect to another host does not carry the token along (reqwest drops it).
pub(super) async fn fetch(
    client: &reqwest::Client,
    url: &reqwest::Url,
    token: Option<&str>,
) -> Result<Vec<u8>> {
    let mut request = client.get(url.clone()).timeout(TIMEOUT);
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let mut response = request.send().await?;
    if !response.status().is_success() {
        return Err(but_github::HttpStatusError {
            status: response.status(),
        }
        .into());
    }
    let too_big = "The image is too big";
    let size = response.content_length().unwrap_or(0);
    ensure!(size <= MAX_BYTES as u64, too_big);
    let mut bytes = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
    while let Some(chunk) = response.chunk().await? {
        bytes.extend_from_slice(&chunk);
        ensure!(bytes.len() <= MAX_BYTES, too_big);
    }
    Ok(bytes)
}

/// Whether `err` is GitHub answering as if a private repository's image weren't there.
pub(super) fn hidden(err: &anyhow::Error) -> bool {
    err.downcast_ref::<but_github::HttpStatusError>()
        .is_some_and(|err| matches!(err.status.as_u16(), 401 | 403 | 404))
}
