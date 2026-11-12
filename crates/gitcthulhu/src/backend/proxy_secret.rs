//! The proxy password (Settings > Network > Proxy; spec 21 §7.1): kept in the OS keychain under the handle
//! `proxy-password`, never in `settings.json`, and merged into the `http.proxy` URL when the Git runtime is built.
//!
//! Without the `services` feature there is no keychain: [`available`] is false and every call fails with
//! [`Unavailable`]. All functions block (keychain access); call them inside `backend::bg`.

use super::RuntimeConfig;
use crate::settings::Settings;

/// Keychain handle of the proxy password.
pub const HANDLE: &str = "proxy-password";

/// The keychain cannot be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unavailable(pub String);

impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Unavailable {}

/// Whether this build can store the password.
pub const fn available() -> bool {
    cfg!(feature = "services")
}

/// Whether a password is stored. Errors: the keychain is unavailable.
#[cfg(feature = "services")]
pub fn exists() -> Result<bool, Unavailable> {
    retrieve().map(|secret| secret.is_some())
}

/// Stores `password`; an empty password deletes the entry. Errors: the keychain is unavailable.
#[cfg(feature = "services")]
pub fn store(password: &str) -> Result<(), Unavailable> {
    let result = if password.is_empty() {
        but_secret::secret::delete(HANDLE, but_secret::secret::Namespace::Global)
    } else {
        but_secret::secret::persist(
            HANDLE,
            &but_secret::Sensitive(password.to_owned()),
            but_secret::secret::Namespace::Global,
        )
    };
    result.map_err(|err| Unavailable(err.to_string()))
}

#[cfg(feature = "services")]
fn retrieve() -> Result<Option<String>, Unavailable> {
    but_secret::secret::retrieve(HANDLE, but_secret::secret::Namespace::Global)
        .map(|secret| secret.map(|s| s.0))
        .map_err(|err| Unavailable(err.to_string()))
}

/// Whether a password is stored. Errors: always, in builds without the keychain.
#[cfg(not(feature = "services"))]
pub fn exists() -> Result<bool, Unavailable> {
    Err(unavailable())
}

/// Stores `password`. Errors: always, in builds without the keychain.
#[cfg(not(feature = "services"))]
pub fn store(_password: &str) -> Result<(), Unavailable> {
    Err(unavailable())
}

#[cfg(not(feature = "services"))]
fn retrieve() -> Result<Option<String>, Unavailable> {
    Err(unavailable())
}

#[cfg(not(feature = "services"))]
fn unavailable() -> Unavailable {
    Unavailable("this build has no secure storage".into())
}

/// Percent-encodes `text` for the userinfo part of a URL (everything but unreserved characters).
pub(crate) fn encode_userinfo(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// `proxy` (`scheme://user@host:port`, the user percent-encoded as `runtime_config` writes it) with `password`
/// after the user, or `proxy` unchanged when it has no `user@` part for `user`.
pub fn with_password(proxy: &str, user: &str, password: &str) -> String {
    let user = encode_userinfo(user);
    let needle = format!("{user}@");
    match proxy.find("://") {
        Some(at) if proxy[at + 3..].starts_with(&needle) && !password.is_empty() => {
            let split = at + 3 + user.len();
            format!(
                "{}:{}{}",
                &proxy[..split],
                encode_userinfo(password),
                &proxy[split..]
            )
        }
        _ => proxy.to_owned(),
    }
}

/// Adds the stored proxy password to the `http.proxy` entry of `config` when `settings` ask for proxy
/// authentication. A missing or unreadable password leaves the entry as it is (Git then asks for the password
/// through the askpass prompt). Blocking.
pub fn apply(config: &mut RuntimeConfig, settings: &Settings) {
    // Unit tests never read the real keychain (runtime rebuilds in tests run `start_runtime`).
    if cfg!(test) || !(settings.use_proxy && settings.proxy_auth && !settings.proxy_user.is_empty())
    {
        return;
    }
    let password = match retrieve() {
        Ok(Some(password)) => password,
        Ok(None) => return,
        Err(err) => {
            tracing::warn!("cannot read the proxy password: {err}");
            return;
        }
    };
    for (key, value) in &mut config.network_config {
        if key == "http.proxy" {
            *value = with_password(value, &settings.proxy_user, &password);
        }
    }
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use super::*;

    #[test]
    fn password_goes_after_the_user_and_is_percent_encoded() {
        assert_eq!(
            with_password("http://alice@proxy.local:3128", "alice", "p@ss:w/rd"),
            "http://alice:p%40ss%3Aw%2Frd@proxy.local:3128"
        );
        assert_eq!(
            with_password("socks5://alice@proxy.local", "alice", "x"),
            "socks5://alice:x@proxy.local"
        );
    }

    #[test]
    fn users_with_url_delimiters_are_encoded_on_both_sides() {
        let settings = Settings {
            use_proxy: true,
            proxy_host: "proxy.local".into(),
            proxy_port: 3128,
            proxy_auth: true,
            proxy_user: r"corp\al@ice:x".into(),
            ..Settings::default()
        };
        let config = crate::backend::runtime_config(&settings);
        let proxy = config
            .network_config
            .iter()
            .find(|(k, _)| k == "http.proxy")
            .map(|(_, v)| v.clone())
            .expect("an http.proxy entry");
        assert!(
            proxy.contains("://corp%5Cal%40ice%3Ax@proxy.local"),
            "the user cannot break the URL: {proxy}"
        );
        assert!(
            with_password(&proxy, &settings.proxy_user, "pw")
                .contains("://corp%5Cal%40ice%3Ax:pw@"),
            "the password goes after the encoded user"
        );
    }

    #[test]
    fn urls_without_the_user_or_without_a_password_stay_unchanged() {
        assert_eq!(
            with_password("http://proxy.local:3128", "alice", "x"),
            "http://proxy.local:3128",
            "no user part"
        );
        assert_eq!(
            with_password("http://alice@proxy.local", "alice", ""),
            "http://alice@proxy.local",
            "an empty password adds nothing"
        );
        assert_eq!(
            with_password("http://bob@proxy.local", "alice", "x"),
            "http://bob@proxy.local",
            "another user"
        );
    }
}
