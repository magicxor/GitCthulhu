//! Parser of `git push --porcelain` output (WP-B8).
//!
//! Git prints, on stdout:
//!
//! ```text
//! To <url>
//! <flag>\t<from>:<to>\t<summary>[ (<reason>)]
//! Done
//! ```
//!
//! with the flags ` ` (fast-forward), `+` (forced), `-` (deleted), `*` (new), `!` (rejected) and `=` (up to date).
//! The parser is deliberately lenient: every line that does not have the `<flag>\t` shape (progress leftovers,
//! `Done`, `branch 'x' set up to track ...`) is ignored.

use crate::{GitError, PushRefStatus, PushReport};

/// One ref line of the porcelain output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PorcelainLine {
    /// The flag character.
    pub flag: char,
    /// The local side; empty for deletions.
    pub from: String,
    /// The remote ref.
    pub to: String,
    /// `[new branch]`, `[rejected]`, `abc1234..def5678`, ... without the reason.
    pub summary: String,
    /// The text in the trailing parentheses, if any.
    pub reason: Option<String>,
}

impl PorcelainLine {
    /// The status the flag stands for.
    pub fn status(&self) -> PushRefStatus {
        match self.flag {
            '+' => PushRefStatus::Forced,
            '-' => PushRefStatus::Deleted,
            '*' => PushRefStatus::New,
            '!' => PushRefStatus::Rejected,
            '=' => PushRefStatus::UpToDate,
            _ => PushRefStatus::FastForward,
        }
    }

    /// The abbreviated `(old, new)` ids of an `old..new` / `old...new` summary.
    pub fn abbreviated_range(&self) -> Option<(&str, &str)> {
        let (old, new) = self
            .summary
            .split_once("...")
            .or_else(|| self.summary.split_once(".."))?;
        let is_hex = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit());
        (is_hex(old) && is_hex(new)).then_some((old, new))
    }
}

/// The parsed output of `git push --porcelain`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PorcelainPush {
    /// The URL of the `To <url>` line.
    pub url: Option<String>,
    /// The ref lines in Git's order.
    pub lines: Vec<PorcelainLine>,
}

/// Parse the stdout of `git push --porcelain`.
pub(crate) fn parse(stdout: &[u8]) -> PorcelainPush {
    let text = String::from_utf8_lossy(stdout);
    let mut parsed = PorcelainPush::default();
    for raw in text.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if let Some(url) = line.strip_prefix("To ") {
            if parsed.url.is_none() {
                parsed.url = Some(url.trim_end().to_owned());
            }
            continue;
        }
        let mut chars = line.chars();
        let (Some(flag), Some('\t')) = (chars.next(), chars.next()) else {
            continue;
        };
        if !matches!(flag, ' ' | '+' | '-' | '*' | '!' | '=') {
            continue;
        }
        let rest = chars.as_str();
        let mut fields = rest.splitn(2, '\t');
        let (Some(refs), Some(tail)) = (fields.next(), fields.next()) else {
            continue;
        };
        let Some((from, to)) = refs.split_once(':') else {
            continue;
        };
        let tail = tail.trim_end();
        // The summary (`[remote rejected]`, `abc..def`, `[new branch]`) never contains " (", but a reason sent by
        // the remote may contain parentheses itself, so split at the first one.
        let (summary, reason) = match tail.strip_suffix(')').and_then(|t| t.split_once(" (")) {
            Some((summary, reason)) => (summary.to_owned(), Some(reason.to_owned())),
            None => (tail.to_owned(), None),
        };
        parsed.lines.push(PorcelainLine {
            flag,
            from: from.to_owned(),
            to: to.to_owned(),
            summary,
            reason,
        });
    }
    parsed
}

/// The error for a push that has at least one rejected (`!`) line, `None` if nothing was rejected.
///
/// The first rejected line decides the class (spec 1.9 rows 11-13): `[rejected]` + `stale info` is a lease
/// rejection (as is `remote ref updated since checkout`, which `--force-if-includes` reports), `[rejected]` +
/// `non-fast-forward` / `fetch first` a non-fast-forward, everything else a remote rejection with the
/// parenthesised text (or the summary) as reason.
pub(crate) fn rejection(
    parsed: &PorcelainPush,
    report: PushReport,
    stderr: String,
) -> Option<GitError> {
    let mut rejected = parsed.lines.iter().filter(|l| l.flag == '!');
    let first = rejected.next()?;
    let refs: Vec<String> = parsed
        .lines
        .iter()
        .filter(|l| l.flag == '!')
        .map(|l| l.to.clone())
        .collect();
    let reason = first.reason.as_deref().unwrap_or("");
    Some(match (first.summary.as_str(), reason) {
        ("[rejected]", "stale info" | "remote ref updated since checkout") => {
            GitError::LeaseRejected {
                refs,
                report,
                stderr,
            }
        }
        ("[rejected]", "non-fast-forward" | "fetch first") => GitError::NonFastForward {
            refs,
            report,
            stderr,
        },
        _ => GitError::RemoteRejected {
            refs,
            reason: first
                .reason
                .clone()
                .unwrap_or_else(|| first.summary.clone()),
            report,
            stderr,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(
        flag: char,
        from: &str,
        to: &str,
        summary: &str,
        reason: Option<&str>,
    ) -> PorcelainLine {
        PorcelainLine {
            flag,
            from: from.into(),
            to: to.into(),
            summary: summary.into(),
            reason: reason.map(Into::into),
        }
    }

    #[test]
    fn porcelain_parser_all_flags() {
        let out = b"To file:///tmp/remote.git\n \trefs/heads/ff:refs/heads/ff\tabc1234..def5678\n\
+\trefs/heads/forced:refs/heads/forced\tabc1234...def5678 (forced update)\n\
-\t:refs/heads/gone\t[deleted]\n\
*\trefs/heads/new:refs/heads/new\t[new branch]\n\
!\trefs/heads/rej:refs/heads/rej\t[rejected] (non-fast-forward)\n\
=\trefs/heads/same:refs/heads/same\t[up to date]\n\
!\trefs/heads/hook:refs/heads/hook\t[remote rejected] (pre-receive hook declined)\r\n\
Done\n";
        let parsed = parse(out);
        assert_eq!(
            parsed.url.as_deref(),
            Some("file:///tmp/remote.git"),
            "the To line gives the url"
        );
        assert_eq!(
            parsed.lines,
            vec![
                line(
                    ' ',
                    "refs/heads/ff",
                    "refs/heads/ff",
                    "abc1234..def5678",
                    None
                ),
                line(
                    '+',
                    "refs/heads/forced",
                    "refs/heads/forced",
                    "abc1234...def5678",
                    Some("forced update")
                ),
                line('-', "", "refs/heads/gone", "[deleted]", None),
                line(
                    '*',
                    "refs/heads/new",
                    "refs/heads/new",
                    "[new branch]",
                    None
                ),
                line(
                    '!',
                    "refs/heads/rej",
                    "refs/heads/rej",
                    "[rejected]",
                    Some("non-fast-forward")
                ),
                line(
                    '=',
                    "refs/heads/same",
                    "refs/heads/same",
                    "[up to date]",
                    None
                ),
                line(
                    '!',
                    "refs/heads/hook",
                    "refs/heads/hook",
                    "[remote rejected]",
                    Some("pre-receive hook declined")
                ),
            ],
            "every flag is parsed, Done is ignored"
        );
        let statuses: Vec<_> = parsed.lines.iter().map(PorcelainLine::status).collect();
        assert_eq!(
            statuses,
            [
                PushRefStatus::FastForward,
                PushRefStatus::Forced,
                PushRefStatus::Deleted,
                PushRefStatus::New,
                PushRefStatus::Rejected,
                PushRefStatus::UpToDate,
                PushRefStatus::Rejected
            ],
            "flags map to statuses"
        );
        assert_eq!(
            parsed.lines[0].abbreviated_range(),
            Some(("abc1234", "def5678")),
            "two-dot range"
        );
        assert_eq!(
            parsed.lines[1].abbreviated_range(),
            Some(("abc1234", "def5678")),
            "three-dot range"
        );
        assert_eq!(
            parsed.lines[3].abbreviated_range(),
            None,
            "no range for new"
        );
    }

    #[test]
    fn rejection_classes() {
        let report = PushReport {
            remote: "origin".into(),
            url: None,
            updates: vec![],
        };
        let parsed = |tail: &str| parse(format!("!\ta:b\t{tail}\n").as_bytes());
        let kind = |tail: &str| {
            rejection(&parsed(tail), report.clone(), String::new())
                .expect("a rejected line yields an error")
                .kind()
        };
        use crate::GitErrorKind as K;
        assert_eq!(
            kind("[rejected] (stale info)"),
            K::LeaseRejected,
            "stale info"
        );
        assert_eq!(
            kind("[rejected] (remote ref updated since checkout)"),
            K::LeaseRejected,
            "force-if-includes"
        );
        assert_eq!(
            kind("[rejected] (fetch first)"),
            K::NonFastForward,
            "fetch first"
        );
        assert_eq!(
            kind("[rejected] (non-fast-forward)"),
            K::NonFastForward,
            "non-ff"
        );
        assert_eq!(
            kind("[remote rejected] (pre-receive hook declined)"),
            K::RemoteRejected,
            "remote rejected"
        );
        assert!(
            rejection(
                &parse(b"*\ta:b\t[new branch]\n"),
                report.clone(),
                String::new()
            )
            .is_none(),
            "no `!` line, no rejection"
        );

        let nested = parsed("[remote rejected] (refusing to update `ci (main).yml` (needs scope))");
        match rejection(&nested, report.clone(), String::new()) {
            Some(GitError::RemoteRejected { reason, refs, .. }) => {
                assert_eq!(
                    reason, "refusing to update `ci (main).yml` (needs scope)",
                    "a reason with parentheses is kept whole"
                );
                assert_eq!(refs, ["b"], "the rejected remote ref");
            }
            other => panic!("expected RemoteRejected, got {other:?}"),
        }
        match rejection(&parsed("[remote failure]"), report, String::new()) {
            Some(GitError::RemoteRejected { reason, .. }) => {
                assert_eq!(
                    reason, "[remote failure]",
                    "without a reason the summary is used"
                );
            }
            other => panic!("expected RemoteRejected, got {other:?}"),
        }
    }
}
