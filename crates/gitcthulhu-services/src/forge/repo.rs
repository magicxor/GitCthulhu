//! [`RepoForge`]: the forge of one repository.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::hash::{Hash as _, Hasher as _};
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use but_db::DbHandle;
use but_forge::{
    CacheConfig, CreateForgeReviewParams, ForgeRepoInfo, ForgeUser, ReviewTemplateFunctions,
};
use gitcthulhu_git::CancelToken;
use sha2::{Digest as _, Sha256};

use super::checkout::{self, ExistingRemote};
use super::checks;
use super::hub::ForgeHub;
use super::{
    CheckoutPlan, ChecksSummary, CiCheck, Conversation, CreateReview, ForgeCapabilities,
    ForgeError, ForgeInfo, ForgeName, ForgeReview, RefreshOutcome, RepoAccountStatus,
    RepoForgeInput, RepoInfo, ReviewAssociation, ReviewMergeMethod, ReviewMergeStatus,
    ReviewUpdatePayload,
};

/// The largest pull request template that is read.
const MAX_TEMPLATE_BYTES: u64 = 1 << 20;

/// The per-repository handle a repository tab holds. Cheap to clone.
///
/// The forge of the repository is detected once, from the base remote and the accounts known when
/// [`ForgeHub::open_repo()`] ran. The review and check caches are one SQLite file per forge repository (so
/// clones and worktrees of the same repository share it); every operation opens its own connection, which
/// keeps a slow refresh from blocking cache reads.
#[derive(Clone)]
pub struct RepoForge {
    inner: Arc<Inner>,
}

struct Inner {
    hub: ForgeHub,
    input: RepoForgeInput,
    /// The accounts known when the handle was opened.
    users: Vec<ForgeUser>,
    /// The repository pull requests target.
    target: Option<ForgeRepoInfo>,
    /// The repository branches are pushed to, when it is not the target (a fork).
    push: Option<ForgeRepoInfo>,
    info: Option<ForgeInfo>,
    /// Where the review and check caches live.
    cache_dir: Option<PathBuf>,
}

impl RepoForge {
    pub(super) fn open(hub: ForgeHub, input: RepoForgeInput) -> Self {
        let users = hub.users().unwrap_or_default();
        let forge_override = input.prefs.forge_override.clone();
        let base_url = input
            .remotes
            .base
            .as_ref()
            .map(|remote| remote.url.as_str());
        let target = base_url
            .and_then(|url| but_forge::derive_forge_repo_info(url, forge_override.clone(), &users));
        let info =
            base_url.and_then(|url| but_forge::forge_info(url, &users, forge_override.clone()));
        let push = match (&input.remotes.base, &input.remotes.push) {
            (Some(base), Some(push)) if base.url != push.url => {
                but_forge::derive_forge_repo_info(&push.url, forge_override, &users)
            }
            _ => None,
        };
        let cache_dir = info
            .as_ref()
            .map(|info| cache_dir(&hub.inner.dirs.cache, info));
        RepoForge {
            inner: Arc::new(Inner {
                hub,
                input,
                users,
                target,
                push,
                info,
                cache_dir,
            }),
        }
    }

    // ---- plumbing ----------------------------------------------------------------------------------

    fn storage(&self) -> &but_forge_storage::Controller {
        &self.inner.hub.inner.storage
    }

    fn preferred(&self) -> &Option<ForgeUser> {
        &self.inner.input.prefs.preferred_user
    }

    /// The repository pull requests target, or `Unrecognized` when the remote belongs to no forge.
    fn target(&self) -> Result<&ForgeRepoInfo, ForgeError> {
        self.inner.target.as_ref().ok_or(ForgeError::Unrecognized)
    }

    /// A fresh connection to the cache of this repository.
    fn open_db(&self) -> Result<DbHandle, ForgeError> {
        let dir = self
            .inner
            .cache_dir
            .as_ref()
            .ok_or(ForgeError::Unrecognized)?;
        DbHandle::new_in_directory(dir).map_err(ForgeError::Other)
    }

    /// Run `future` on the shared runtime, blocking the calling thread.
    fn run<F: Future>(&self, future: F) -> F::Output {
        self.inner.hub.inner.rt.block_on(future)
    }

    /// Run `future` on the shared runtime after the other forge refreshes, dropping it when `cancel` fires.
    fn run_refresh<F, T>(&self, cancel: &CancelToken, future: F) -> Result<T, ForgeError>
    where
        F: Future<Output = Result<T, ForgeError>>,
    {
        let hub = &self.inner.hub.inner;
        hub.rt.block_on_cancellable(cancel, async {
            // One refresh at a time per hub; waiting for the turn is cancellable too.
            let _one_at_a_time = hub.refresh.lock().await;
            future.await
        })?
    }

    fn number(number: i64) -> Result<usize, ForgeError> {
        usize::try_from(number).map_err(|_| ForgeError::NotFound)
    }

    // ---- detection ---------------------------------------------------------------------------------

    /// The detected forge; `None` = "No forge detected" (offer the override picker).
    pub fn info(&self) -> Option<&ForgeInfo> {
        self.inner.info.as_ref()
    }

    /// What the forge supports.
    pub fn capabilities(&self) -> ForgeCapabilities {
        self.inner
            .info
            .as_ref()
            .map(|info| info.capabilities.clone())
            .unwrap_or(ForgeCapabilities {
                checks: false,
                repo_info: false,
                pr_service: false,
                list_service: false,
                review_comments: false,
                review_management: false,
            })
    }

    /// Which account would be used (no network, no keychain). Azure DevOps counts as `NotAForge`: it is
    /// detected for its web URLs only and has no API integration.
    pub fn account_status(&self) -> RepoAccountStatus {
        let Some(target) = &self.inner.target else {
            return RepoAccountStatus::NotAForge;
        };
        if target.forge == ForgeName::Azure {
            return RepoAccountStatus::NotAForge;
        }
        match but_forge::find_forge_account(self.preferred(), target, self.storage()) {
            Ok(Some(user)) => RepoAccountStatus::Matching(self.inner.hub.account(user)),
            Ok(None) | Err(_) => RepoAccountStatus::NoMatchingAccount,
        }
    }

    // ---- URLs --------------------------------------------------------------------------------------

    fn base_url(&self) -> Option<&str> {
        self.inner
            .input
            .remotes
            .base
            .as_ref()
            .map(|remote| remote.url.as_str())
    }

    /// Web URL of a commit.
    pub fn url_commit(&self, sha: &str) -> Option<String> {
        let info = self.info()?;
        Some(format!("{}{}{sha}", info.base_url, info.commit_url_path))
    }

    /// Web URL of a pull request.
    pub fn url_review(&self, number: i64) -> Option<String> {
        let info = self.info()?;
        Some(format!("{}{}{number}", info.base_url, info.pr_url_path))
    }

    /// Web URL that starts a new pull request from `head_branch` into `base` (fork-aware: with a push
    /// remote that is another repository, the head is the fork's branch).
    pub fn url_new_review(&self, base: &str, head_branch: &str) -> Option<String> {
        let target = self.inner.target.as_ref()?;
        let fork = self
            .inner
            .push
            .as_ref()
            .filter(|push| !push.same_repository(target))
            .map(|push| push.owner.as_str());
        but_forge::compare_branch_url(
            self.base_url()?,
            base,
            head_branch,
            fork,
            &self.inner.users,
            self.inner.input.prefs.forge_override.clone(),
        )
    }

    /// Web URL of a branch of the repository branches are pushed to.
    pub fn url_branch(&self, branch: &str) -> Option<String> {
        let push_url = self
            .inner
            .input
            .remotes
            .push
            .as_ref()
            .map(|remote| remote.url.as_str());
        but_forge::branch_url(
            push_url.or_else(|| self.base_url())?,
            branch,
            &self.inner.users,
            self.inner.input.prefs.forge_override.clone(),
        )
    }

    // ---- reviews -----------------------------------------------------------------------------------

    /// The open reviews in the cache (no network).
    pub fn cached_reviews(&self) -> Result<Vec<ForgeReview>, ForgeError> {
        self.target()?;
        let db = self.open_db()?;
        Ok(but_forge::list_cached_forge_reviews(&db)?
            .into_iter()
            .filter(ForgeReview::is_open)
            .collect())
    }

    /// Every remote-tracking branch of `repo` (`origin/feature`) that has a review in the cache, with that
    /// review: number, whether it is open, and the commit it merged at. One pass over the cache, for badges
    /// on all branches at once; a fork's reviews are matched to the remotes that fetch from or push to the fork.
    pub fn review_associations(
        &self,
        repo: &gix::Repository,
    ) -> Result<HashMap<String, ReviewAssociation>, ForgeError> {
        if self.inner.target.is_none() {
            return Ok(HashMap::new());
        }
        let db = self.open_db()?;
        Ok(but_forge::review_associations_by_head(
            &db,
            repo,
            &self.inner.users,
        )?)
    }

    /// The review of a remote branch (e.g. `origin/feature`), from the cache. For many branches use
    /// [`Self::review_associations()`]: this reads the cache each time.
    pub fn review_for_branch(
        &self,
        repo: &gix::Repository,
        remote_branch: &str,
    ) -> Option<ReviewAssociation> {
        self.review_associations(repo).ok()?.remove(remote_branch)
    }

    /// Refresh the review list from the forge into the cache, after the other refreshes of this hub.
    ///
    /// Cancelling drops the request and leaves the cache as it was.
    pub fn refresh_reviews(&self, cancel: &CancelToken) -> Result<RefreshOutcome, ForgeError> {
        let target = self.target()?;
        self.run_refresh(cancel, async {
            let mut db = self.open_db()?;
            but_forge::list_forge_reviews_with_cache(
                self.preferred().clone(),
                target,
                self.storage(),
                &mut db,
                Some(CacheConfig::NoCache),
            )
            .await?;
            let cached = but_forge::list_cached_forge_reviews(&db)?;
            Ok(RefreshOutcome {
                fingerprint: fingerprint(&cached),
                last_merged: last_merged(&cached),
            })
        })
    }

    /// One pull request, from the forge; it is written to the cache.
    pub fn review(&self, number: i64) -> Result<ForgeReview, ForgeError> {
        let target = self.target()?;
        let number = Self::number(number)?;
        self.run(async {
            let mut db = self.open_db()?;
            Ok(but_forge::get_forge_review(
                self.preferred(),
                target,
                number,
                &mut db,
                self.storage(),
            )
            .await?)
        })
    }

    /// Open a pull request, from the fork when the push remote is another repository. The new review is
    /// cached right away, so it shows before the next refresh.
    pub fn create_review(&self, p: CreateReview) -> Result<ForgeReview, ForgeError> {
        let target = self.target()?;
        let params = CreateForgeReviewParams {
            title: p.title,
            body: p.body,
            source_branch: p.source_branch,
            target_branch: p.target_branch,
            draft: p.draft,
        };
        self.run(async {
            let review = but_forge::create_forge_review(
                self.preferred(),
                target,
                &self.inner.push,
                &params,
                self.storage(),
            )
            .await?;
            // The review exists on the forge now: a cache that cannot take it must not fail the call.
            match self.open_db() {
                Ok(mut db) => {
                    if let Err(err) = but_forge::cache_review(&mut db, &review) {
                        tracing::warn!("Could not cache the new review: {err:#}");
                    }
                }
                Err(err) => tracing::warn!("Could not open the review cache: {err}"),
            }
            Ok(review)
        })
    }

    /// Change title, body, target and state of a pull request.
    pub fn update_review(&self, number: i64, u: ReviewUpdatePayload) -> Result<(), ForgeError> {
        let target = self.target()?;
        let number = Self::number(number)?;
        self.run(async {
            Ok(
                but_forge::update_review(self.preferred(), target, number, u, self.storage())
                    .await?,
            )
        })
    }

    /// Merge a pull request. `None` merges the way the forge's default does.
    pub fn merge_review(
        &self,
        number: i64,
        m: Option<ReviewMergeMethod>,
    ) -> Result<(), ForgeError> {
        let target = self.target()?;
        let number = Self::number(number)?;
        self.run(async {
            Ok(
                but_forge::merge_review(self.preferred(), target, number, m, self.storage())
                    .await?,
            )
        })
    }

    /// Turn auto-merge on or off (GitHub and GitLab; Bitbucket reports an error).
    pub fn set_auto_merge(
        &self,
        number: i64,
        on: bool,
        m: Option<ReviewMergeMethod>,
    ) -> Result<(), ForgeError> {
        let target = self.target()?;
        let number = Self::number(number)?;
        self.run(async {
            Ok(but_forge::set_review_auto_merge_state(
                self.preferred(),
                target,
                number,
                on,
                m,
                self.storage(),
            )
            .await?)
        })
    }

    /// Mark a pull request as draft or ready for review.
    pub fn set_draft(&self, number: i64, draft: bool) -> Result<(), ForgeError> {
        let target = self.target()?;
        let number = Self::number(number)?;
        self.run(async {
            Ok(but_forge::set_review_draftiness(
                self.preferred(),
                target,
                number,
                draft,
                self.storage(),
            )
            .await?)
        })
    }

    /// Whether a pull request can be merged, and why not. Always asks the forge.
    pub fn merge_status(&self, number: i64) -> Result<ReviewMergeStatus, ForgeError> {
        let target = self.target()?;
        let number = Self::number(number)?;
        self.run(async {
            Ok(
                but_forge::get_review_merge_status(
                    self.preferred(),
                    target,
                    number,
                    self.storage(),
                )
                .await?,
            )
        })
    }

    /// Comments, threads, submissions and reactions of a pull request, read at the same time. Only GitHub
    /// has a conversation; GitLab and Bitbucket answer an empty one.
    pub fn conversation(&self, number: i64) -> Result<Conversation, ForgeError> {
        let target = self.target()?;
        let number = Self::number(number)?;
        let (preferred, storage) = (self.preferred(), self.storage());
        self.run(async {
            let (comments, threads, submissions, reactions) = tokio::try_join!(
                but_forge::list_review_comments(preferred, target, number, storage),
                but_forge::list_review_threads(preferred, target, number, storage),
                but_forge::list_review_submissions(preferred, target, number, storage),
                but_forge::list_review_reactions(preferred, target, number, storage),
            )?;
            Ok(Conversation {
                comments,
                threads,
                submissions,
                reactions,
            })
        })
    }

    // ---- templates ---------------------------------------------------------------------------------

    /// The pull request templates found in `worktree`, relative to it.
    pub fn templates(&self, worktree: &Path) -> Vec<String> {
        match self.info() {
            Some(info) => but_forge::available_review_templates(worktree, &info.name),
            None => Vec::new(),
        }
    }

    /// Read the template `rel` (relative to `worktree`): a path the forge accepts as a template, inside the
    /// worktree, up to 1 MiB of UTF-8.
    pub fn read_template(&self, worktree: &Path, rel: &str) -> Result<String, ForgeError> {
        let info = self.info().ok_or(ForgeError::Unrecognized)?;
        let ReviewTemplateFunctions {
            is_valid_review_template_path,
            ..
        } = but_forge::get_review_template_functions(&info.name);
        let rel_path = Path::new(rel);
        let relative = rel_path
            .components()
            .all(|part| matches!(part, Component::Normal(_)));
        if !relative || !is_valid_review_template_path(rel_path) {
            return Err(ForgeError::other(format!(
                "'{rel}' is not a pull request template of this forge"
            )));
        }
        // A symlink must not lead out of the worktree.
        let root = std::fs::canonicalize(worktree).map_err(|e| ForgeError::Other(e.into()))?;
        let file = std::fs::canonicalize(root.join(rel_path)).map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => ForgeError::NotFound,
            _ => ForgeError::Other(e.into()),
        })?;
        if !file.starts_with(&root) {
            return Err(ForgeError::other(format!(
                "'{rel}' leads out of the repository"
            )));
        }
        // Read at most one byte past the cap, so a file that grows after a size check cannot slip through.
        let mut bytes = Vec::new();
        std::fs::File::open(&file)
            .and_then(|f| f.take(MAX_TEMPLATE_BYTES + 1).read_to_end(&mut bytes))
            .map_err(|e| ForgeError::Other(e.into()))?;
        if bytes.len() as u64 > MAX_TEMPLATE_BYTES {
            return Err(ForgeError::other(format!(
                "'{rel}' is too big for a template"
            )));
        }
        String::from_utf8(bytes)
            .map_err(|_| ForgeError::other(format!("'{rel}' is not valid UTF-8")))
    }

    // ---- misc --------------------------------------------------------------------------------------

    /// The default pull-request target: the remote's default branch (from
    /// `gitcthulhu_git::remote::default_branch`), else `"main"`.
    pub fn default_review_target(&self, remote_default_branch: Option<&str>) -> String {
        remote_default_branch
            .map(str::trim)
            .filter(|branch| !branch.is_empty())
            .unwrap_or("main")
            .to_owned()
    }

    /// Permissions, fork status and merge settings of the repository on the forge.
    pub fn repo_info(&self) -> Result<RepoInfo, ForgeError> {
        let target = self.target()?;
        self.run(async {
            Ok(but_forge::get_repo_info(self.preferred(), target, self.storage()).await?)
        })
    }

    /// How to check pull request `number` out locally. The review comes from the cache, else from the
    /// forge. The remotes of the repository are read from disk.
    pub fn checkout_plan(&self, number: i64) -> Result<CheckoutPlan, ForgeError> {
        let target = self.target()?;
        let review = match self.cached_review(number) {
            Some(review) => review,
            None => self.review(number)?,
        };
        let existing = self.existing_remotes()?;
        checkout::plan(
            &review,
            self.inner.input.remotes.base.as_ref(),
            target,
            &existing,
            &self.inner.users,
        )
    }

    fn cached_review(&self, number: i64) -> Option<ForgeReview> {
        let db = self.open_db().ok()?;
        but_forge::list_cached_forge_reviews(&db)
            .ok()?
            .into_iter()
            .find(|review| review.number == number)
    }

    fn existing_remotes(&self) -> Result<Vec<ExistingRemote>, ForgeError> {
        let repo =
            gix::open(&self.inner.input.repo_dir).map_err(|e| ForgeError::Other(e.into()))?;
        Ok(repo
            .remote_names()
            .iter()
            .map(|name| {
                let urls = repo
                    .find_remote(&**name)
                    .map(|remote| {
                        remote
                            .urls(gix::remote::Direction::Fetch)
                            .map(|url| url.to_bstring().to_string())
                            .collect()
                    })
                    .unwrap_or_default();
                ExistingRemote {
                    name: name.to_string(),
                    urls,
                }
            })
            .collect())
    }

    // ---- CI ----------------------------------------------------------------------------------------

    /// The CI checks of `reference` in the cache (no network).
    pub fn cached_checks(&self, reference: &str) -> Result<Vec<CiCheck>, ForgeError> {
        let target = self.target()?;
        self.run(async {
            let mut db = self.open_db()?;
            Ok(but_forge::ci_checks_for_ref_with_cache(
                self.preferred().clone(),
                target,
                self.storage(),
                reference,
                None,
                &mut db,
                Some(CacheConfig::CacheOnly),
            )
            .await?)
        })
    }

    /// Refresh the CI checks of `reference` from the forge into the cache, after the other refreshes of this
    /// hub. `head_commit` is what GitHub is asked about instead of the branch, as for a pull request from a
    /// fork (whose branch the base repository does not have).
    pub fn refresh_checks(
        &self,
        reference: &str,
        head_commit: Option<&str>,
        cancel: &CancelToken,
    ) -> Result<Vec<CiCheck>, ForgeError> {
        let target = self.target()?;
        self.run_refresh(cancel, async {
            let mut db = self.open_db()?;
            Ok(but_forge::ci_checks_for_ref_with_cache(
                self.preferred().clone(),
                target,
                self.storage(),
                reference,
                head_commit,
                &mut db,
                Some(CacheConfig::NoCache),
            )
            .await?)
        })
    }

    /// Count `checks` by state.
    pub fn checks_summary(checks: &[CiCheck]) -> ChecksSummary {
        checks::summarize(checks)
    }

    /// Drop the cached checks of every reference not in `keep_refs`, e.g. of branches that are gone.
    pub fn prune_checks(&self, keep_refs: &HashSet<String>) -> Result<(), ForgeError> {
        self.target()?;
        let mut db = self.open_db()?;
        let cached = db
            .ci_checks()
            .list_all_references()
            .map_err(|e| ForgeError::Other(e.into()))?;
        for reference in cached.iter().filter(|name| !keep_refs.contains(*name)) {
            db.ci_checks_mut()
                .and_then(|checks| checks.delete_for_reference(reference))
                .map_err(|e| ForgeError::Other(e.into()))?;
        }
        Ok(())
    }
}

/// The directory of the caches of the repository `info` describes. Its web URL identifies the forge
/// repository the same way for every remote URL form (SSH or HTTPS, aliases), so clones and worktrees of one
/// repository share the cache, and a repository whose remote was changed never mixes in the old one's rows.
fn cache_dir(cache_root: &Path, info: &ForgeInfo) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(format!("{:?}|{}", info.name, info.base_url.to_lowercase()));
    let digest = hasher.finalize();
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    cache_root.join("forge").join(hex)
}

/// A fingerprint of the reviews the cache knows: unchanged means nothing visible changed. Order-free.
/// Includes when a review changed rather than its description, which can be long.
fn fingerprint(reviews: &[ForgeReview]) -> u64 {
    reviews.iter().fold(0, |sum, review| {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (
            review.number,
            &review.title,
            review.draft,
            review.auto_merge_enabled,
            (&review.merged_at, &review.closed_at),
            &review.modified_at,
        )
            .hash(&mut hasher);
        sum ^ hasher.finish()
    })
}

/// Unix time of the most recent merge among `reviews`.
fn last_merged(reviews: &[ForgeReview]) -> Option<i64> {
    reviews
        .iter()
        .filter_map(|review| {
            let merged = review.merged_at.as_deref()?;
            chrono::DateTime::parse_from_rfc3339(merged)
                .ok()
                .map(|at| at.timestamp())
        })
        .max()
}
