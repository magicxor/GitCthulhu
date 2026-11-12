//! Reading refs: branches, remotes, tags, stashes, decorations (WP-B5). All reads run in-process with gix.

use std::{
    cmp::Ordering,
    collections::{BTreeMap, HashMap, HashSet},
    path::PathBuf,
};

use bstr::{BStr, ByteSlice as _};
use gix::{ObjectId, refs::FullName, remote::Direction};

use crate::{
    AheadBehind, CancelToken, Decorations, GitError, GitTime, HeadState, LocalBranch, RefLabel,
    RefList, RefListOptions, RemoteBranch, RemoteInfo, Repo, Result, StashInfo, TagInfo, Upstream,
    config::other, remote::symref_head_branch, worktree::head_state,
};

impl RefListOptions {
    /// Remotes, tags and stashes.
    pub const ALL: RefListOptions = RefListOptions {
        remotes: true,
        tags: true,
        stashes: true,
    };
}

fn lossy(name: &BStr) -> String {
    name.to_str_lossy().into_owned()
}

fn git_time(time: gix::date::Time) -> GitTime {
    GitTime {
        seconds: time.seconds,
        offset_minutes: time.offset / 60,
    }
}

/// All refs of the repository.
pub fn list(repo: &Repo, opts: RefListOptions) -> Result<RefList> {
    let gix_repo = repo.local();
    let head = head_state(&gix_repo)?;

    // Branches checked out in the other worktrees.
    let mut checked_out: HashMap<FullName, PathBuf> = HashMap::new();
    for entry in crate::worktree::list(repo)? {
        if entry.is_current || entry.bare {
            continue;
        }
        if let Some(HeadState::Branch { name, .. } | HeadState::Unborn { name }) = entry.head {
            checked_out.entry(name).or_insert(entry.path);
        }
    }

    let platform = gix_repo.references().map_err(other)?;

    let mut local = Vec::new();
    for reference in platform.local_branches().map_err(other)? {
        let mut reference = reference.map_err(other)?;
        let full = reference.name().to_owned();
        let Ok(target) = reference.peel_to_id() else {
            continue;
        };
        let name = full.shorten().to_str_lossy().into_owned();
        let upstream = upstream_of(&gix_repo, &full, &name)?;
        local.push(LocalBranch {
            is_head: matches!(&head, HeadState::Branch { name, .. } | HeadState::Unborn { name } if *name == full),
            checked_out_in: checked_out.get(&full).cloned(),
            name,
            full,
            target: target.detach(),
            upstream,
            ahead_behind: None,
        });
    }
    local.sort_by(|a, b| a.name.cmp(&b.name));

    let mut remotes = Vec::new();
    if opts.remotes {
        remotes = remote_infos(&gix_repo, &platform)?;
    }

    let mut tags = Vec::new();
    let mut undecorated_tags = HashSet::new();
    if opts.tags {
        for reference in platform.tags().map_err(other)? {
            let reference = reference.map_err(other)?;
            let Some((info, is_commit)) = tag_info(&gix_repo, reference)? else {
                continue;
            };
            if !is_commit {
                undecorated_tags.insert(info.full.clone());
            }
            tags.push(info);
        }
        tags.sort_by(|a, b| version_cmp(&a.name, &b.name));
    }

    let stashes = if opts.stashes {
        stash_list(repo)?
    } else {
        Vec::new()
    };

    let mut list = RefList {
        head,
        local,
        remotes,
        tags,
        stashes,
        decorations: Decorations::default(),
    };
    list.decorations = build_decorations(&list, &undecorated_tags);
    Ok(list)
}

fn upstream_of(
    gix_repo: &gix::Repository,
    full: &FullName,
    short: &str,
) -> Result<Option<Upstream>> {
    let Some(remote) = gix_repo.branch_remote_name(short.as_bytes().as_bstr(), Direction::Fetch)
    else {
        return Ok(None);
    };
    let remote = remote.as_bstr().to_str_lossy().into_owned();
    let Some(merge) = gix_repo.branch_remote_ref_name(full.as_ref(), Direction::Fetch) else {
        return Ok(None);
    };
    let merge = merge.map_err(other)?;
    let tracking = if remote == "." {
        // Upstream is a local branch.
        Some(merge.clone())
    } else {
        gix_repo
            .branch_remote_tracking_ref_name(full.as_ref(), Direction::Fetch)
            .and_then(|name| name.ok())
    };
    let gone = match &tracking {
        Some(tracking) => gix_repo
            .try_find_reference(tracking.as_ref())
            .map_err(other)?
            .is_none(),
        None => false,
    };
    Ok(Some(Upstream {
        remote,
        merge,
        tracking,
        gone,
    }))
}

/// One remote's fetch destination patterns, to assign remote-tracking refs to it.
struct RemoteMap {
    name: String,
    /// `(prefix, suffix)` of `refs/remotes/<r>/*` style destinations, or the exact name (no `*`).
    patterns: Vec<(String, Option<String>)>,
}

impl RemoteMap {
    /// Score (prefix length) and the matched branch name.
    fn matches(&self, full: &str) -> Option<(usize, String)> {
        let mut best: Option<(usize, String)> = None;
        for (prefix, suffix) in &self.patterns {
            let candidate = match suffix {
                Some(suffix) => full
                    .strip_prefix(prefix.as_str())
                    .and_then(|rest| rest.strip_suffix(suffix.as_str()))
                    .filter(|name| !name.is_empty())
                    .map(|name| (prefix.len(), name.to_owned())),
                None if full == prefix => {
                    let own = format!("refs/remotes/{}/", self.name);
                    let name = full
                        .strip_prefix(own.as_str())
                        .or_else(|| full.rsplit('/').next())
                        .unwrap_or(full);
                    Some((prefix.len(), name.to_owned()))
                }
                None => None,
            };
            if let Some(candidate) = candidate
                && best.as_ref().is_none_or(|(score, _)| candidate.0 > *score)
            {
                best = Some(candidate);
            }
        }
        best
    }
}

fn remote_infos(
    gix_repo: &gix::Repository,
    platform: &gix::reference::iter::Platform<'_>,
) -> Result<Vec<RemoteInfo>> {
    let mut infos: BTreeMap<String, RemoteInfo> = BTreeMap::new();
    let mut maps = Vec::new();
    for name in gix_repo.remote_names() {
        let name = name.to_str_lossy().into_owned();
        let Ok(remote) = gix_repo.find_remote(name.as_str()) else {
            continue;
        };
        let url = |direction| {
            remote
                .url(direction)
                .map(|url| url.to_bstring().to_str_lossy().into_owned())
        };
        let patterns = remote
            .refspecs(Direction::Fetch)
            .iter()
            .filter_map(|spec| spec.to_ref().local().map(lossy))
            .map(|dst| match dst.split_once('*') {
                Some((prefix, suffix)) => (prefix.to_owned(), Some(suffix.to_owned())),
                None => (dst, None),
            })
            .collect();
        maps.push(RemoteMap {
            name: name.clone(),
            patterns,
        });
        infos.insert(
            name.clone(),
            RemoteInfo {
                fetch_url: url(Direction::Fetch),
                push_url: url(Direction::Push),
                head_branch: symref_head_branch(gix_repo, &name),
                branches: Vec::new(),
                name,
            },
        );
    }
    // Longest name first: the fallback matches `refs/remotes/<name>/` prefixes.
    let mut by_name_len: Vec<&String> = infos.keys().collect();
    by_name_len.sort_by_key(|name| std::cmp::Reverse(name.len()));
    let by_name_len: Vec<String> = by_name_len.into_iter().cloned().collect();

    for reference in platform.remote_branches().map_err(other)? {
        let mut reference = reference.map_err(other)?;
        let full = reference.name().to_owned();
        let full_str = lossy(full.as_bstr());
        let Some(rest) = full_str.strip_prefix("refs/remotes/") else {
            continue;
        };
        let (remote, branch) = match maps
            .iter()
            .filter_map(|map| {
                map.matches(&full_str)
                    .map(|(s, n)| (s, map.name.clone(), n))
            })
            .max_by_key(|(score, ..)| *score)
        {
            Some((_, remote, branch)) => (remote, branch),
            None => match by_name_len.iter().find_map(|name| {
                rest.strip_prefix(name.as_str())
                    .and_then(|r| r.strip_prefix('/'))
                    .map(|branch| (name.clone(), branch.to_owned()))
            }) {
                Some(found) => found,
                // A leftover of a deleted remote: group by the first path component.
                None => match rest.split_once('/') {
                    Some((remote, branch)) => (remote.to_owned(), branch.to_owned()),
                    None => continue,
                },
            },
        };
        if branch == "HEAD" {
            continue;
        }
        let Ok(target) = reference.peel_to_id() else {
            continue;
        };
        infos
            .entry(remote.clone())
            .or_insert_with(|| RemoteInfo {
                name: remote.clone(),
                fetch_url: None,
                push_url: None,
                head_branch: None,
                branches: Vec::new(),
            })
            .branches
            .push(RemoteBranch {
                remote,
                name: branch,
                full,
                target: target.detach(),
            });
    }
    let mut remotes: Vec<RemoteInfo> = infos.into_values().collect();
    for remote in &mut remotes {
        remote.branches.sort_by(|a, b| a.name.cmp(&b.name));
    }
    Ok(remotes)
}

/// `(info, points_to_commit)`; `None` for a ref that cannot be resolved.
fn tag_info(
    gix_repo: &gix::Repository,
    mut reference: gix::Reference<'_>,
) -> Result<Option<(TagInfo, bool)>> {
    let full = reference.name().to_owned();
    let name = full.shorten().to_str_lossy().into_owned();
    // `peel_to_id` rewrites the reference in place: remember the unpeeled id (the tag object) first.
    let first = match reference.target() {
        gix::refs::TargetRef::Object(id) => Some(id.to_owned()),
        gix::refs::TargetRef::Symbolic(_) => None,
    };
    let Ok(direct) = reference.peel_to_id() else {
        return Ok(None);
    };
    let first = first.unwrap_or_else(|| direct.detach());
    let object = gix_repo.find_object(first).map_err(other)?;
    let (tag_object, summary, tagger_time, target, is_commit) =
        if object.kind == gix::object::Kind::Tag {
            let tag = object.clone().into_tag();
            let decoded = tag.decode().map_err(other)?;
            let summary = decoded
                .message
                .lines()
                .next()
                .map(|line| lossy(line.trim().as_bstr()))
                .filter(|line| !line.is_empty());
            let time = decoded
                .tagger()
                .ok()
                .flatten()
                .and_then(|sig| sig.time().ok())
                .map(git_time);
            let peeled = object.peel_tags_to_end().map_err(other)?;
            (
                Some(first),
                summary,
                time,
                peeled.id,
                peeled.kind == gix::object::Kind::Commit,
            )
        } else {
            (
                None,
                None,
                None,
                object.id,
                object.kind == gix::object::Kind::Commit,
            )
        };
    Ok(Some((
        TagInfo {
            name,
            full,
            target,
            tag_object,
            message_summary: summary,
            tagger_time,
        },
        is_commit,
    )))
}

/// Digit runs compare numerically, other runs bytewise (`v1.9` < `v1.10`).
fn version_cmp(a: &str, b: &str) -> Ordering {
    fn runs(s: &str) -> Vec<(bool, &str)> {
        let mut out = Vec::new();
        let mut start = 0;
        let mut kind = None;
        for (i, c) in s.char_indices() {
            let is_digit = c.is_ascii_digit();
            if kind != Some(is_digit) {
                if kind.is_some() {
                    out.push((kind == Some(true), &s[start..i]));
                }
                start = i;
                kind = Some(is_digit);
            }
        }
        if kind.is_some() {
            out.push((kind == Some(true), &s[start..]));
        }
        out
    }
    for (x, y) in runs(a).into_iter().zip(runs(b)) {
        let ord = match (x.0, y.0) {
            (true, true) => {
                let (xt, yt) = (x.1.trim_start_matches('0'), y.1.trim_start_matches('0'));
                xt.len().cmp(&yt.len()).then_with(|| xt.cmp(yt))
            }
            _ => x.1.cmp(y.1),
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

/// Ahead/behind per (local, upstream tracking) pair; walks at most `cap` commits per side ("999+" in the UI).
pub fn ahead_behind(
    repo: &Repo,
    pairs: &[(ObjectId, ObjectId)],
    cap: u32,
    cancel: &CancelToken,
) -> Result<Vec<AheadBehind>> {
    let gix_repo = repo.local();
    let count = |tip: ObjectId, hidden: ObjectId| -> Result<(u32, bool)> {
        let walk = gix_repo
            .rev_walk([tip])
            .with_hidden([hidden])
            .use_commit_graph(true)
            .all()
            .map_err(other)?;
        let mut n = 0u32;
        for info in walk {
            if n >= cap {
                return Ok((n, true));
            }
            info.map_err(other)?;
            n += 1;
            if n.is_multiple_of(256) {
                cancel.check()?;
            }
        }
        Ok((n, false))
    };
    let mut out = Vec::with_capacity(pairs.len());
    for &(local, upstream) in pairs {
        cancel.check()?;
        if local == upstream {
            out.push(AheadBehind {
                ahead: 0,
                behind: 0,
                capped: false,
            });
            continue;
        }
        let (ahead, ahead_capped) = count(local, upstream)?;
        let (behind, behind_capped) = count(upstream, local)?;
        out.push(AheadBehind {
            ahead,
            behind,
            capped: ahead_capped || behind_capped,
        });
    }
    Ok(out)
}

/// The branch a stash was made on, from `WIP on <b>: ...` / `On <b>: ...`.
fn stash_branch(message: &str) -> Option<String> {
    let rest = message
        .strip_prefix("WIP on ")
        .or_else(|| message.strip_prefix("On "))?;
    let (branch, _) = rest.split_once(": ")?;
    (branch != "(no branch)").then(|| branch.to_owned())
}

/// The stash list, `stash@{0}` first.
pub fn stash_list(repo: &Repo) -> Result<Vec<StashInfo>> {
    let gix_repo = repo.local();
    let Some(reference) = gix_repo.try_find_reference("refs/stash").map_err(other)? else {
        return Ok(Vec::new());
    };
    let mut platform = reference.log_iter();
    let Some(lines) = platform.rev().map_err(|source| GitError::Io {
        path: repo.common_dir().join("logs").join("refs").join("stash"),
        source,
    })?
    else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for (index, line) in lines.enumerate() {
        let line = line.map_err(other)?;
        let id = line.new_oid;
        let Ok(commit) = gix_repo.find_commit(id) else {
            continue;
        };
        let decoded = commit.decode().map_err(other)?;
        let mut parents = decoded.parents();
        let Some(base) = parents.next() else {
            continue;
        };
        let message = decoded
            .message
            .lines()
            .next()
            .map(|l| lossy(l.as_bstr()))
            .unwrap_or_else(|| lossy(line.message.as_bstr()));
        let time = decoded
            .committer()
            .ok()
            .and_then(|sig| sig.time().ok())
            .map(git_time)
            .unwrap_or(GitTime {
                seconds: 0,
                offset_minutes: 0,
            });
        out.push(StashInfo {
            index,
            id,
            branch: stash_branch(&message),
            message,
            time,
            base,
            index_commit: parents.next(),
            untracked_commit: parents.next(),
        });
    }
    Ok(out)
}

/// rev-parse like `git rev-parse --verify <spec>^{commit}`.
pub fn resolve_commit(repo: &Repo, spec: &str) -> Result<ObjectId> {
    let not_found = || GitError::NotFound {
        what: "commit",
        name: spec.to_owned(),
    };
    let gix_repo = repo.local();
    let id = gix_repo
        .rev_parse_single(spec.as_bytes().as_bstr())
        .map_err(|_| not_found())?;
    let commit = id
        .object()
        .map_err(other)?
        .peel_to_commit()
        .map_err(|_| not_found())?;
    Ok(commit.id)
}

/// Is `branch` fully merged into `into` (HEAD if None)? For the "delete unmerged branch?" prompt.
pub fn is_merged(repo: &Repo, branch: ObjectId, into: Option<ObjectId>) -> Result<bool> {
    let gix_repo = repo.local();
    let into = match into {
        Some(into) => into,
        None => match head_state(&gix_repo)? {
            HeadState::Branch { target, .. } | HeadState::Detached { target } => target,
            HeadState::Unborn { .. } => return Ok(false),
        },
    };
    if branch == into {
        return Ok(true);
    }
    let base = gix_repo.merge_base(branch, into).map_err(other)?;
    Ok(base.is_some_and(|base| base.detach() == branch))
}

/// Labels per commit; pure, also filled into `RefList::decorations` by `list`.
pub fn decorations(list: &RefList) -> Decorations {
    build_decorations(list, &HashSet::new())
}

/// `skip_tags`: full names of tags that do not point at commits.
fn build_decorations(list: &RefList, skip_tags: &HashSet<FullName>) -> Decorations {
    let mut map: HashMap<ObjectId, smallvec::SmallVec<[RefLabel; 2]>> = HashMap::new();
    let mut add = |id: ObjectId, label: RefLabel| map.entry(id).or_default().push(label);
    if let HeadState::Branch { target, .. } | HeadState::Detached { target } = &list.head {
        add(*target, RefLabel::Head);
    }
    for branch in &list.local {
        add(branch.target, RefLabel::LocalBranch(branch.name.clone()));
    }
    for remote in &list.remotes {
        for branch in &remote.branches {
            add(
                branch.target,
                RefLabel::RemoteBranch {
                    remote: branch.remote.clone(),
                    name: branch.name.clone(),
                },
            );
        }
    }
    for tag in list.tags.iter().filter(|t| !skip_tags.contains(&t.full)) {
        add(tag.target, RefLabel::Tag(tag.name.clone()));
    }
    for stash in &list.stashes {
        add(stash.id, RefLabel::Stash(stash.index));
    }
    Decorations(map)
}
