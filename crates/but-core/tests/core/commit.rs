mod create {
    use anyhow::Context as _;
    use bstr::ByteSlice;
    use but_core::commit::{self, SignCommit};
    use but_error::Code;
    use but_testsupport::{writable_scenario, writable_scenario_with_ssh_key};
    use gix::{objs::commit::SIGNATURE_FIELD_NAME, refs};

    #[test]
    fn signs_commits_when_enabled() -> anyhow::Result<()> {
        let (repo, _tmp) = writable_scenario_with_ssh_key("single-signed");

        let oid = commit::create(
            &repo,
            commit_from_head(&repo, "signed again")?,
            None,
            SignCommit::IfSignCommitsEnabled,
        )?;
        let commit = repo.find_commit(oid)?;
        let commit = commit.decode()?;

        assert!(
            commit.extra_headers().find(SIGNATURE_FIELD_NAME).is_some(),
            "a new signature should be written, the new commit needs to be signed"
        );
        Ok(())
    }

    #[test]
    fn sign_commit_yes_forcefully_signs_commit() -> anyhow::Result<()> {
        let (mut repo, _tmp) = writable_scenario_with_ssh_key("single-signed");
        repo.config_snapshot_mut()
            .set_raw_value("commit.gpgSign", "false")?;

        let oid = commit::create(
            &repo,
            commit_from_head(&repo, "should sign")?,
            None,
            SignCommit::Yes,
        )?;
        let commit = repo.find_commit(oid)?;
        let commit = commit.decode()?;

        assert!(
            commit.extra_headers().pgp_signature().is_some(),
            "despite the settings saying everything is disabled, it still signs the commit"
        );
        Ok(())
    }

    #[test]
    fn sign_commit_yes_without_signing_key_errors() -> anyhow::Result<()> {
        let (repo, _tmp) = writable_scenario("single-unsigned");

        let err = commit::create(
            &repo,
            commit_from_head(&repo, "should not sign")?,
            None,
            SignCommit::Yes,
        )
        .expect_err("commit creation should fail as there is no signing key configured");

        let error_code: Option<&Code> = err.downcast_ref();
        assert_eq!(error_code, Some(&Code::CommitSigningFailed));
        Ok(())
    }

    #[test]
    fn sign_commit_if_enabled_without_signing_key_errors_and_keeps_config() -> anyhow::Result<()> {
        let (mut repo, _tmp) = writable_scenario_with_ssh_key("single-signed");
        repo.config_snapshot_mut()
            .set_raw_value("user.signingKey", "BAD-key")?;

        let err = commit::create(
            &repo,
            commit_from_head(&repo, "should not sign")?,
            None,
            SignCommit::IfSignCommitsEnabled,
        )
        .expect_err("commit creation should fail as there is no signing key configured");

        let error_code: Option<&Code> = err.downcast_ref();
        assert_eq!(error_code, Some(&Code::CommitSigningFailed));

        repo.reload()?;
        assert_eq!(
            repo.config_snapshot().boolean("commit.gpgSign"),
            Some(true),
            "a signing failure never changes the user's Git configuration"
        );

        Ok(())
    }

    #[test]
    fn removes_existing_signature_when_not_signing() -> anyhow::Result<()> {
        let (repo, _tmp) = writable_scenario_with_ssh_key("single-signed");
        assert!(
            repo.head_commit()?
                .decode()?
                .extra_headers()
                .find(SIGNATURE_FIELD_NAME)
                .is_some(),
            "the fixture starts with a signed commit"
        );

        let oid = commit::create(
            &repo,
            commit_from_head(&repo, "unsigned")?,
            None,
            SignCommit::No,
        )?;
        let commit = repo.find_commit(oid)?;
        let commit = commit.decode()?;

        assert!(
            commit.extra_headers().find(SIGNATURE_FIELD_NAME).is_none(),
            "old signatures should be stripped when signing is disabled, as they would be invalid otherwise"
        );
        Ok(())
    }

    #[test]
    fn updates_the_requested_reference() -> anyhow::Result<()> {
        let (repo, _tmp) = writable_scenario_with_ssh_key("single-signed");
        let update_ref =
            refs::Category::LocalBranch.to_full_name("created-by-helper".as_bytes().as_bstr())?;

        let oid = commit::create(
            &repo,
            commit_from_head(&repo, "updates ref")?,
            Some(update_ref.as_ref()),
            SignCommit::No,
        )?;
        let reference = repo.find_reference(update_ref.as_ref())?;

        assert_eq!(
            reference
                .try_id()
                .context("newly created direct reference")?,
            oid
        );
        let previous = repo.find_reference("refs/heads/main")?.id();
        assert_ne!(previous, oid, "the helper should not rewrite other refs");
        Ok(())
    }

    #[test]
    fn keeps_existing_headers_and_encoding() -> anyhow::Result<()> {
        let (repo, _tmp) = writable_scenario_with_ssh_key("single-signed");
        let mut new_commit = commit_from_head(&repo, "preserves metadata")?;
        new_commit.encoding = Some("ISO-8859-1".into());
        new_commit
            .extra_headers
            .push(("x-test-header".into(), "kept".into()));

        let oid = commit::create(&repo, new_commit, None, SignCommit::No)?;
        let commit = repo.find_commit(oid)?;
        let commit = commit.decode()?;

        assert_eq!(commit.encoding, Some("ISO-8859-1".as_bytes().as_bstr()));
        assert_eq!(
            commit.extra_headers().find("x-test-header"),
            Some("kept".as_bytes().as_bstr())
        );
        assert!(
            commit.extra_headers().find(SIGNATURE_FIELD_NAME).is_none(),
            "the inherited signature should still be removed"
        );
        Ok(())
    }

    fn commit_from_head(
        repo: &gix::Repository,
        message: &str,
    ) -> anyhow::Result<gix::objs::Commit> {
        let head = repo.head_commit()?;
        let mut commit = head.decode()?.to_owned()?;
        commit.tree = head.tree_id()?.detach();
        commit.parents = [head.id].into();
        commit.message = message.into();
        Ok(commit)
    }
}
