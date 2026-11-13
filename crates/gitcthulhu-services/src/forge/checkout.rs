//! Checking a pull request out: which remote to fetch from, what to fetch, what to call the branch.
//!
//! [`plan()`] is pure: the caller reads the repository's remotes and the review. The GUI executes the plan with
//! the Git backend (add the remote if it is new, fetch, create the branch at the fetched commit).
//! Remote naming follows the old `review_apply` glue: a remote that already fetches from the pull request's
//! repository is reused, otherwise a new one is named after the repository's owner.

use gitcthulhu_git::RemoteRef;

use super::{CheckoutPlan, ForgeError, ForgeName, ForgeRepoInfo, ForgeReview, ForgeUser};

/// A remote of the repository, as far as the plan needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ExistingRemote {
    pub(super) name: String,
    /// Its fetch URLs.
    pub(super) urls: Vec<String>,
}

/// The plan to check out `review`.
///
/// * `base` is the remote pull requests target and `base_info` its repository.
/// * `existing` are all remotes of the repository.
/// * `accounts` are the known accounts, needed to recognise self-hosted remote URLs.
pub(super) fn plan(
    review: &ForgeReview,
    base: Option<&RemoteRef>,
    base_info: &ForgeRepoInfo,
    existing: &[ExistingRemote],
    accounts: &[ForgeUser],
) -> Result<CheckoutPlan, ForgeError> {
    let number = review.number;
    let source = review.source_branch.trim();
    let head_url = head_url(review, &base_info.protocol)
        .or_else(|| same_repository_head(review, base, base_info));
    let (Some(head_url), false) = (head_url, source.is_empty()) else {
        return pull_ref_plan(review, base, base_info);
    };

    let (remote_name, remote_url) = match existing_remote_for(existing, &head_url, accounts) {
        Some(found) => found,
        None => {
            let owner = review
                .repo_owner
                .as_deref()
                .or_else(|| review.author.as_ref().map(|author| author.login.as_str()))
                .unwrap_or_default();
            let name = unique_remote_name(existing, &sanitize_remote_name(owner, number));
            (name, head_url)
        }
    };
    let tracking = format!("refs/remotes/{remote_name}/{source}");
    // The head of a pull request inside the repository is a branch like any other; the branch of a fork's
    // pull request is namespaced so that it cannot clash with a local branch of the same name.
    let local_branch = if review.head_repo_is_fork {
        let owner = review.repo_owner.as_deref().unwrap_or_default();
        format!("{}-{source}", sanitize_remote_name(owner, number))
    } else {
        source.to_owned()
    };
    Ok(CheckoutPlan {
        fetch_refspec: format!("+refs/heads/{source}:{tracking}"),
        remote_name,
        remote_url,
        local_branch,
        fetched_ref: tracking.clone(),
        upstream: Some(tracking),
    })
}

/// The plan for a pull request whose head branch cannot be named (its repository was deleted, or the forge
/// does not say where it is): fetch the pull request's own ref from the base remote. GitHub and GitLab
/// publish one; Bitbucket does not.
fn pull_ref_plan(
    review: &ForgeReview,
    base: Option<&RemoteRef>,
    base_info: &ForgeRepoInfo,
) -> Result<CheckoutPlan, ForgeError> {
    let number = review.number;
    let (server_ref, prefix) = match base_info.forge {
        ForgeName::GitHub => (format!("refs/pull/{number}/head"), "pr"),
        ForgeName::GitLab => (format!("refs/merge-requests/{number}/head"), "mr"),
        ForgeName::Bitbucket | ForgeName::Azure => {
            return Err(ForgeError::other(format!(
                "Review {number} cannot be checked out: its source branch is not available and {:?} publishes no review refs",
                base_info.forge
            )));
        }
    };
    let base = base.ok_or_else(|| {
        ForgeError::other("The repository has no remote to fetch the review from")
    })?;
    Ok(CheckoutPlan {
        remote_name: base.name.clone(),
        remote_url: base.url.clone(),
        fetch_refspec: format!("+{server_ref}:{server_ref}"),
        local_branch: format!("{prefix}-{number}"),
        fetched_ref: server_ref,
        upstream: None,
    })
}

/// The clone URL of the review's head repository, SSH when the base remote uses SSH, else HTTPS.
fn head_url(review: &ForgeReview, base_protocol: &str) -> Option<String> {
    let prefers_ssh = base_protocol.to_ascii_lowercase().contains("ssh");
    let (first, second) = if prefers_ssh {
        (&review.repository_ssh_url, &review.repository_https_url)
    } else {
        (&review.repository_https_url, &review.repository_ssh_url)
    };
    first
        .clone()
        .or_else(|| second.clone())
        .filter(|url| !url.trim().is_empty())
}

/// The base remote's URL when the review's head is the base repository itself although the forge names no
/// clone URL for it, as Bitbucket never does: the review is not from a fork and its source repository's owner
/// is the base repository's. A review whose head repository is unknown (GitHub reports a deleted fork as
/// no repository and no owner) does not qualify: its branch is not in the base repository.
fn same_repository_head(
    review: &ForgeReview,
    base: Option<&RemoteRef>,
    base_info: &ForgeRepoInfo,
) -> Option<String> {
    let owner = review.repo_owner.as_deref()?;
    (!review.head_repo_is_fork && owner.eq_ignore_ascii_case(&base_info.owner))
        .then(|| base.map(|base| base.url.clone()))
        .flatten()
}

/// The remote that already fetches from the repository `url` names, with the URL it fetches from.
fn existing_remote_for(
    existing: &[ExistingRemote],
    url: &str,
    accounts: &[ForgeUser],
) -> Option<(String, String)> {
    existing.iter().find_map(|remote| {
        remote
            .urls
            .iter()
            .find(|configured| urls_match(configured, url, accounts))
            .map(|configured| (remote.name.clone(), configured.clone()))
    })
}

/// Whether two remote URLs name the same repository.
fn urls_match(configured: &str, candidate: &str, accounts: &[ForgeUser]) -> bool {
    if configured == candidate {
        return true;
    }
    // Two arbitrary URLs are compared for reuse, so the repository's forge override does not apply.
    let configured = but_forge::derive_forge_repo_info(configured, None, accounts);
    let candidate = but_forge::derive_forge_repo_info(candidate, None, accounts);
    match (configured, candidate) {
        (Some(configured), Some(candidate)) => configured.same_repository(&candidate),
        _ => false,
    }
}

/// A remote name made of `input` (a repository owner): lower case, letters, digits, `.`, `_` and `-`, no
/// leading or trailing punctuation. `pr-<number>` when nothing is left (or only the reserved `head`).
fn sanitize_remote_name(input: &str, number: i64) -> String {
    let mut out = String::new();
    let mut last_was_dash = false;
    for ch in input.chars().flat_map(char::to_lowercase) {
        let safe = ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-');
        let next = if safe { ch } else { '-' };
        if next == '-' {
            if !last_was_dash {
                out.push(next);
            }
            last_was_dash = true;
        } else {
            out.push(next);
            last_was_dash = false;
        }
    }
    let out = out.trim_matches(|ch| matches!(ch, '.' | '_' | '-'));
    if out.is_empty() || out == "head" {
        format!("pr-{number}")
    } else {
        out.to_owned()
    }
}

/// `base`, or `base-2`, `base-3`, ... when a remote of that name exists.
fn unique_remote_name(existing: &[ExistingRemote], base: &str) -> String {
    let taken = |name: &str| existing.iter().any(|remote| remote.name == name);
    let mut candidate = base.to_owned();
    let mut suffix = 2;
    while taken(&candidate) {
        candidate = format!("{base}-{suffix}");
        suffix += 1;
    }
    candidate
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::ForgeReviewUser;

    fn base_info(protocol: &str) -> ForgeRepoInfo {
        ForgeRepoInfo {
            forge: ForgeName::GitHub,
            host: "github.com".into(),
            port: None,
            owner: "acme".into(),
            repo: "widgets".into(),
            protocol: protocol.into(),
        }
    }

    fn base() -> RemoteRef {
        RemoteRef {
            name: "origin".into(),
            url: "https://github.com/acme/widgets.git".into(),
        }
    }

    fn remote(name: &str, url: &str) -> ExistingRemote {
        ExistingRemote {
            name: name.into(),
            urls: vec![url.into()],
        }
    }

    fn review() -> ForgeReview {
        ForgeReview {
            html_url: "https://github.com/acme/widgets/pull/7".into(),
            number: 7,
            title: "Fix".into(),
            body: None,
            author: Some(ForgeReviewUser {
                id: 1,
                login: "Octo Cat".into(),
                name: None,
                email: None,
                avatar_url: None,
                is_bot: false,
            }),
            labels: Vec::new(),
            draft: false,
            source_branch: "fix-it".into(),
            target_branch: "main".into(),
            sha: String::new(),
            integration_commit_shas: Vec::new(),
            created_at: None,
            modified_at: None,
            merged_at: None,
            closed_at: None,
            repository_ssh_url: Some("git@github.com:octocat/widgets.git".into()),
            repository_https_url: Some("https://github.com/octocat/widgets.git".into()),
            repo_owner: Some("octocat".into()),
            head_repo_is_fork: true,
            reviewers: Vec::new(),
            auto_merge_enabled: false,
            unit_symbol: "#".into(),
            last_sync_at: chrono::NaiveDateTime::default(),
        }
    }

    #[test]
    fn a_fork_gets_a_remote_named_after_its_owner() {
        let plan = plan(
            &review(),
            Some(&base()),
            &base_info("https"),
            &[remote("origin", &base().url)],
            &[],
        )
        .unwrap();
        assert_eq!(
            plan,
            CheckoutPlan {
                remote_name: "octocat".into(),
                remote_url: "https://github.com/octocat/widgets.git".into(),
                fetch_refspec: "+refs/heads/fix-it:refs/remotes/octocat/fix-it".into(),
                local_branch: "octocat-fix-it".into(),
                fetched_ref: "refs/remotes/octocat/fix-it".into(),
                upstream: Some("refs/remotes/octocat/fix-it".into()),
            },
            "a new remote named after the fork's owner, a branch that cannot clash with local ones"
        );
    }

    #[test]
    fn the_remote_url_follows_the_protocol_of_the_base_remote() {
        let plan = plan(
            &review(),
            Some(&base()),
            &base_info("ssh"),
            &[remote("origin", &base().url)],
            &[],
        )
        .unwrap();
        assert_eq!(
            plan.remote_url, "git@github.com:octocat/widgets.git",
            "an SSH base remote gets an SSH fork remote"
        );
    }

    #[test]
    fn an_existing_remote_of_the_same_repository_is_reused() {
        for (existing_url, why) in [
            ("https://github.com/octocat/widgets.git", "the same URL"),
            (
                "git@github.com:octocat/widgets",
                "the same repository by another URL form",
            ),
        ] {
            let plan = plan(
                &review(),
                Some(&base()),
                &base_info("https"),
                &[
                    remote("origin", &base().url),
                    remote("friend", existing_url),
                ],
                &[],
            )
            .unwrap();
            assert_eq!(plan.remote_name, "friend", "reuse the remote with {why}");
            assert_eq!(
                plan.remote_url, existing_url,
                "the URL stays the way it is configured ({why})"
            );
            assert_eq!(
                plan.fetch_refspec, "+refs/heads/fix-it:refs/remotes/friend/fix-it",
                "the refspec fills the reused remote's tracking namespace"
            );
        }
    }

    #[test]
    fn a_pull_request_inside_the_repository_uses_the_base_remote_and_its_branch_name() {
        let mut inside = review();
        inside.head_repo_is_fork = false;
        inside.repo_owner = Some("acme".into());
        inside.repository_https_url = Some("https://github.com/acme/widgets.git".into());
        inside.repository_ssh_url = Some("git@github.com:acme/widgets.git".into());
        let plan = plan(
            &inside,
            Some(&base()),
            &base_info("https"),
            &[remote("origin", &base().url)],
            &[],
        )
        .unwrap();
        assert_eq!(plan.remote_name, "origin", "the base remote fetches it");
        assert_eq!(plan.local_branch, "fix-it", "no namespacing without a fork");
        assert_eq!(
            plan.upstream.as_deref(),
            Some("refs/remotes/origin/fix-it"),
            "the new branch tracks the remote branch"
        );
    }

    #[test]
    fn a_bitbucket_review_inside_the_repository_uses_the_base_remote() {
        // Bitbucket names no clone URLs; its review says only who owns the source repository.
        let mut inside = review();
        inside.repository_https_url = None;
        inside.repository_ssh_url = None;
        inside.head_repo_is_fork = false;
        inside.repo_owner = Some("ACME".into());
        let mut info = base_info("https");
        info.forge = ForgeName::Bitbucket;
        let base = RemoteRef {
            name: "origin".into(),
            url: "https://bitbucket.org/acme/widgets.git".into(),
        };
        let plan = plan(
            &inside,
            Some(&base),
            &info,
            &[remote("origin", &base.url)],
            &[],
        )
        .expect("the branch is in the base repository");
        assert_eq!(
            plan,
            CheckoutPlan {
                remote_name: "origin".into(),
                remote_url: base.url.clone(),
                fetch_refspec: "+refs/heads/fix-it:refs/remotes/origin/fix-it".into(),
                local_branch: "fix-it".into(),
                fetched_ref: "refs/remotes/origin/fix-it".into(),
                upstream: Some("refs/remotes/origin/fix-it".into()),
            },
            "the base remote fetches the branch, which then tracks it"
        );

        let mut fork = inside.clone();
        fork.head_repo_is_fork = true;
        fork.repo_owner = Some("contributor".into());
        assert!(
            super::plan(
                &fork,
                Some(&base),
                &info,
                &[remote("origin", &base.url)],
                &[]
            )
            .is_err(),
            "a Bitbucket fork has no clone URL and no review ref: it cannot be planned"
        );
        let mut unknown = inside;
        unknown.repo_owner = None;
        assert!(
            super::plan(
                &unknown,
                Some(&base),
                &info,
                &[remote("origin", &base.url)],
                &[]
            )
            .is_err(),
            "without a source repository nothing says the branch is in the base repository"
        );
    }

    #[test]
    fn a_taken_remote_name_gets_a_suffix() {
        let plan = plan(
            &review(),
            Some(&base()),
            &base_info("https"),
            &[
                remote("origin", &base().url),
                remote("octocat", "https://example.org/somebody/else.git"),
                remote("octocat-2", "https://example.org/somebody/other.git"),
            ],
            &[],
        )
        .unwrap();
        assert_eq!(
            plan.remote_name, "octocat-3",
            "a different repository must not be fetched through an existing name"
        );
    }

    #[test]
    fn owner_names_become_safe_remote_names() {
        for (owner, expected) in [
            ("Octo Cat", "octo-cat"),
            ("--weird__", "weird"),
            ("a//b", "a-b"),
            ("", "pr-7"),
            ("head", "pr-7"),
            ("Ünï", "n"),
        ] {
            assert_eq!(
                sanitize_remote_name(owner, 7),
                expected,
                "remote name for owner '{owner}'"
            );
        }
    }

    #[test]
    fn without_a_head_repository_the_pull_ref_of_the_base_remote_is_fetched() {
        let mut gone = review();
        gone.repository_https_url = None;
        gone.repository_ssh_url = None;
        gone.repo_owner = None;
        let github = plan(&gone, Some(&base()), &base_info("https"), &[], &[]).unwrap();
        assert_eq!(
            github,
            CheckoutPlan {
                remote_name: "origin".into(),
                remote_url: base().url,
                fetch_refspec: "+refs/pull/7/head:refs/pull/7/head".into(),
                local_branch: "pr-7".into(),
                fetched_ref: "refs/pull/7/head".into(),
                upstream: None,
            },
            "a review ref is no remote-tracking branch: nothing to set as upstream"
        );
        let mut info = base_info("https");
        info.forge = ForgeName::GitLab;
        let gitlab = plan(&gone, Some(&base()), &info, &[], &[]).unwrap();
        assert_eq!(
            gitlab.fetch_refspec, "+refs/merge-requests/7/head:refs/merge-requests/7/head",
            "GitLab keeps merge request heads in refs/merge-requests"
        );
        info.forge = ForgeName::Bitbucket;
        assert!(
            plan(&gone, Some(&base()), &info, &[], &[]).is_err(),
            "Bitbucket has no review refs to fall back to"
        );
        assert!(
            plan(&gone, None, &base_info("https"), &[], &[]).is_err(),
            "without a base remote there is nothing to fetch from"
        );
    }
}
