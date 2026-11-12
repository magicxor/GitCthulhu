//! Forge, AI and update services for the GitCthulhu GUI (20 §8).
//!
//! One crate, one shared [`Runtime`] (2 workers) for all network services; every API is blocking and meant
//! to be called from background threads. The modules `forge`, `ai` and `update` are cargo features of the same
//! name (all on by default).

pub mod runtime;
pub mod types;

#[cfg(feature = "ai")]
pub mod ai;
#[cfg(feature = "forge")]
pub mod forge;
#[cfg(feature = "update")]
pub mod update;

pub use gitcthulhu_git::CancelToken;
pub use runtime::{Cancelled, Runtime};
pub use types::{ServicesConfig, UpdateChannel};

/// The secret-store namespace of the app (keychain entries are prefixed with it).
pub const SECRET_NAMESPACE: &str = "gitcthulhu";

/// All services, sharing one runtime.
pub struct Services {
    /// The shared runtime.
    pub runtime: Runtime,
    /// Forge integrations (GitHub, GitLab, Bitbucket).
    #[cfg(feature = "forge")]
    pub forge: forge::ForgeHub,
    /// AI text generation.
    #[cfg(feature = "ai")]
    pub ai: ai::AiService,
    /// Update checks.
    #[cfg(feature = "update")]
    pub update: update::UpdateService,
}

impl Services {
    /// Set the secret namespace to `gitcthulhu`, start the shared runtime and build the enabled services.
    pub fn new(cfg: ServicesConfig) -> anyhow::Result<Services> {
        but_secret::secret::set_application_namespace(SECRET_NAMESPACE);
        let runtime = Runtime::new()?;
        let _ = &cfg; // unused when neither `forge` nor `update` is enabled
        #[cfg(feature = "forge")]
        let forge = forge::ForgeHub::new(
            runtime.clone(),
            forge::ForgeDirs {
                app_data: cfg.app_data_dir.clone(),
                cache: cfg.cache_dir.clone(),
            },
            forge::ForgeConfig {
                github_oauth_client_id: cfg.github_oauth_client_id.clone(),
                user_agent: cfg.user_agent.clone(),
            },
        )?;
        #[cfg(feature = "ai")]
        let ai = ai::AiService::new(runtime.clone());
        #[cfg(feature = "update")]
        let update = update::UpdateService::new(runtime.clone(), &cfg);
        Ok(Services {
            runtime,
            #[cfg(feature = "forge")]
            forge,
            #[cfg(feature = "ai")]
            ai,
            #[cfg(feature = "update")]
            update,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The GUI shares `Arc<Services>` (and clones of the individual services) across background threads.
    #[test]
    fn services_are_send_and_sync() {
        fn assert_send_sync<T: Send + Sync + 'static>() {}
        assert_send_sync::<Services>();
        assert_send_sync::<Runtime>();
        assert_send_sync::<Cancelled>();
        #[cfg(feature = "forge")]
        {
            assert_send_sync::<forge::ForgeHub>();
            assert_send_sync::<forge::RepoForge>();
            assert_send_sync::<forge::ForgeError>();
        }
        #[cfg(feature = "ai")]
        {
            assert_send_sync::<ai::AiService>();
            assert_send_sync::<ai::AiError>();
        }
        #[cfg(feature = "update")]
        {
            assert_send_sync::<update::UpdateService>();
            assert_send_sync::<update::UpdateError>();
        }
    }

    #[test]
    fn services_new_sets_secret_namespace_gitcthulhu() {
        but_secret::secret::set_application_namespace("not-gitcthulhu");
        let dir = std::env::temp_dir().join("gitcthulhu-services-test");
        let services = Services::new(ServicesConfig {
            app_data_dir: dir.join("data"),
            cache_dir: dir.join("cache"),
            user_agent: "GitCthulhu/0.0.0".into(),
            app_version: semver::Version::new(0, 0, 0),
            github_oauth_client_id: None,
            update_manifest_url: None,
            update_channel: UpdateChannel::Release,
        })
        .expect("services start");
        assert_eq!(
            but_secret::secret::application_namespace(),
            "gitcthulhu",
            "Services::new switches the keychain namespace to the app identity"
        );
        drop(services);
    }
}
