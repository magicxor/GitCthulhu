/// A module to bundle configuration we *write* per repository, but read as normal.
pub mod git {
    use std::ffi::OsString;

    use anyhow::Result;
    use bstr::{BString, ByteSlice};

    use crate::git_config::{edit_repo_config, remove_config_value};

    const GIT_SIGN_COMMITS: &str = "commit.gpgsign";
    const SIGNING_KEY: &str = "user.signingKey";
    const SIGNING_FORMAT: &str = "gpg.format";
    const GPG_PROGRAM: &str = "gpg.program";
    const GPG_SSH_PROGRAM: &str = "gpg.ssh.program";

    pub(crate) mod types {
        use std::ffi::OsString;

        use bstr::BString;

        /// Commit-signing settings that are retrieved from Git and written into the repository-local configuration.
        ///
        /// Only standard Git keys are used.
        #[derive(Debug, PartialEq, Clone, Default)]
        pub struct GitConfigSettings {
            /// `commit.gpgSign`: if `true`, commits should be signed.
            /// This value is always set when querying it, and defaults to `false` just like Git would.
            pub sign_commits: Option<bool>,
            /// `user.signingKey`.
            pub signing_key: Option<BString>,
            /// `gpg.format`
            pub signing_format: Option<BString>,
            /// `gpg.program`
            pub gpg_program: Option<OsString>,
            /// `gpg.ssh.program`
            pub gpg_ssh_program: Option<OsString>,
        }
    }
    use types::GitConfigSettings;

    impl GitConfigSettings {
        /// Read all settings from the given snapshot.
        pub fn try_from_snapshot(config: &gix::config::Snapshot<'_>) -> anyhow::Result<Self> {
            let sign_commits = config.boolean(GIT_SIGN_COMMITS).or(Some(false));
            let signing_key = config.string(SIGNING_KEY);
            let signing_format = config.string(SIGNING_FORMAT);
            let gpg_program = config.trusted_program(GPG_PROGRAM)?;
            let gpg_ssh_program = config.trusted_program(GPG_SSH_PROGRAM)?;
            Ok(GitConfigSettings {
                sign_commits,
                signing_key,
                signing_format,
                gpg_program,
                gpg_ssh_program,
            })
        }

        /// Write our data back to the local `.git/config` file of the given `repo`.
        ///
        /// Values that are `None` are left untouched, empty values remove the key.
        pub fn persist_to_local_config(&self, repo: &gix::Repository) -> Result<()> {
            // TODO: make this easier in `gix`. Could use config-snapshot-mut, but there is no way to
            //       auto-reload it/assure it's up-to-date.
            edit_repo_config(repo, gix::config::Source::Local, |config| {
                if let Some(sign_commits) = self.sign_commits {
                    config.set_raw_value(
                        GIT_SIGN_COMMITS,
                        if sign_commits { "true" } else { "false" },
                    )?;
                };
                if let Some(signing_key) = &self.signing_key {
                    if signing_key.is_empty() {
                        remove_config_value(config, SIGNING_KEY)?;
                    } else {
                        config.set_raw_value(SIGNING_KEY, signing_key.as_bstr())?;
                    }
                };
                if let Some(signing_format) = &self.signing_format {
                    if signing_format.is_empty() {
                        remove_config_value(config, SIGNING_FORMAT)?;
                    } else {
                        config.set_raw_value(SIGNING_FORMAT, signing_format.as_bstr())?;
                    }
                }
                if let Some(gpg_program) = self.gpg_program.as_ref().and_then(osstring_into_bstring)
                {
                    if gpg_program.is_empty() {
                        remove_config_value(config, GPG_PROGRAM)?;
                    } else {
                        config.set_raw_value(GPG_PROGRAM, gpg_program.as_bstr())?;
                    }
                }
                if let Some(gpg_ssh_program) = self
                    .gpg_ssh_program
                    .as_ref()
                    .and_then(osstring_into_bstring)
                {
                    if gpg_ssh_program.is_empty() {
                        remove_config_value(config, GPG_SSH_PROGRAM)?;
                    } else {
                        config.set_raw_value(GPG_SSH_PROGRAM, gpg_ssh_program.as_bstr())?;
                    }
                }

                Ok(())
            })?;
            Ok(())
        }
    }

    fn osstring_into_bstring(s: &OsString) -> Option<BString> {
        match gix::path::os_str_into_bstr(s) {
            Ok(s) => Some(s.to_owned()),
            Err(err) => {
                tracing::warn!("Could not convert to string due to illegal UTF8: {err}");
                None
            }
        }
    }
}
