mod git {
    use but_core::{GitConfigSettings, RepositoryExt};
    use but_testsupport::gix_testtools;

    #[test]
    fn set_git_settings() -> anyhow::Result<()> {
        let tmp = gix_testtools::tempfile::TempDir::new()?;
        gix::init(tmp.path())?;
        let repo = gix::open_opts(tmp.path(), gix::open::Options::isolated())?;
        let actual = repo.git_settings()?;
        assert_eq!(
            actual,
            GitConfigSettings {
                sign_commits: Some(false),
                ..Default::default()
            },
            "by default, None of these are set in a new repository, except for the explicit gpg-sign logic"
        );
        let expected = GitConfigSettings {
            sign_commits: Some(true),
            signing_key: Some("signing key".into()),
            signing_format: Some("signing format".into()),
            gpg_program: Some("gpg program".into()),
            gpg_ssh_program: Some("gpg ssh program".into()),
        };
        repo.set_git_settings(&expected)?;
        let actual = repo.git_settings()?;

        assert_ne!(
            actual, expected,
            "round-tripping isn't possible due to the way this works - it would need mutability."
        );

        let repo = but_testsupport::open_repo(repo.path())?;
        let actual = repo.git_settings()?;
        assert_eq!(
            actual, expected,
            "but it works once the settings are reloaded, they were persisted to disk."
        );
        assert_eq!(
            repo.config_snapshot().boolean("commit.gpgSign"),
            Some(true),
            "signing is the standard Git setting"
        );
        Ok(())
    }

    #[test]
    fn set_partial_git_settings() -> anyhow::Result<()> {
        let tmp = gix_testtools::tempfile::TempDir::new()?;
        gix::init(tmp.path())?;
        let repo = gix::open_opts(tmp.path(), gix::open::Options::isolated())?;
        let expected = GitConfigSettings {
            sign_commits: Some(true),
            ..Default::default()
        };

        // need a reload, see `set_git_settings` for details on why.
        repo.set_git_settings(&expected)?;

        let repo = but_testsupport::open_repo(repo.path())?;
        let actual = repo.git_settings()?;
        assert_eq!(
            actual, expected,
            "it only writes what is given (as changed)"
        );

        Ok(())
    }

    #[test]
    fn empty_strings_remove_existing_values() -> anyhow::Result<()> {
        let tmp = gix_testtools::tempfile::TempDir::new()?;
        gix::init(tmp.path())?;
        let repo = gix::open_opts(tmp.path(), gix::open::Options::isolated())?;

        repo.set_git_settings(&GitConfigSettings {
            sign_commits: Some(true),
            signing_key: Some("signing key".into()),
            signing_format: Some("ssh".into()),
            gpg_program: Some("gpg".into()),
            gpg_ssh_program: Some("ssh-keygen".into()),
        })?;

        repo.set_git_settings(&GitConfigSettings {
            sign_commits: Some(true),
            signing_key: Some("".into()),
            signing_format: Some("".into()),
            gpg_program: Some("".into()),
            gpg_ssh_program: Some("".into()),
        })?;

        let repo = but_testsupport::open_repo(repo.path())?;
        let actual = repo.git_settings()?;
        assert_eq!(actual.signing_key, None);
        assert_eq!(actual.signing_format, None);
        assert_eq!(actual.gpg_program, None);
        assert_eq!(actual.gpg_ssh_program, None);

        let config = repo.config_snapshot();
        assert!(
            config.string("user.signingKey").is_none(),
            "expected empty signing key to remove the config key"
        );
        assert!(
            config.string("gpg.format").is_none(),
            "expected empty signing format to remove the config key"
        );
        assert!(
            config.trusted_program("gpg.program")?.is_none(),
            "expected empty gpg program to remove the config key"
        );
        assert!(
            config.trusted_program("gpg.ssh.program")?.is_none(),
            "expected empty gpg ssh program to remove the config key"
        );

        Ok(())
    }

    #[test]
    fn setting_one_value_preserves_other_config() -> anyhow::Result<()> {
        let tmp = gix_testtools::tempfile::TempDir::new()?;
        gix::init(tmp.path())?;
        let repo = gix::open_opts(tmp.path(), gix::open::Options::isolated())?;
        repo.set_git_settings(&GitConfigSettings {
            signing_key: Some("existing-key".into()),
            ..Default::default()
        })?;
        repo.set_git_settings(&GitConfigSettings {
            signing_format: Some("ssh".into()),
            ..Default::default()
        })?;

        let repo = but_testsupport::open_repo(repo.path())?;
        let settings = repo.git_settings()?;
        assert_eq!(settings.signing_key, Some("existing-key".into()));
        assert_eq!(settings.signing_format, Some("ssh".into()));
        Ok(())
    }
}
