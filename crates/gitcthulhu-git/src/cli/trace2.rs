//! Reader of `GIT_TRACE2_EVENT` output to attribute hook failures (§1.11).
//!
//! The executor points `GIT_TRACE2_EVENT` at a fresh directory, so every git process writes one file of JSON lines
//! there, named after the last component of its session id (`sid`). A child git's sid is `<parent sid>/<own>`, where
//! `<own>` ends in `-P<pid as 8 hex digits>`.
//!
//! Hooks inherit `GIT_TRACE2_EVENT`, so nested `git` calls inside hooks (lint-staged, husky) write their own files
//! into the same directory and must not be attributed to the command. A file counts if
//! - its sid contains no `/` (the top-level git process), or
//! - its parent's file counts and the parent started it as a **non-hook** child: the parent's `child_exit` event
//!   for that child carries the pid that ends the child's sid. This keeps hooks of git sub-commands that Git runs
//!   itself (`git pull` runs `git merge`, which runs `post-merge`), while a `git` started *by a hook* has the hook's
//!   shell as its parent, never a pid of the parent's git children.
//!
//! Unparseable files and lines are ignored: a trace2 problem must never fail the operation.

use std::{
    collections::{HashMap, HashSet},
    path::Path,
};

use crate::HookRun;

/// Whether a failing hook means the operation did not happen (pre) or did happen (post).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HookClass {
    /// The operation did not happen: `GitError::HookFailed`.
    Pre,
    /// The operation happened: `OpWarning::PostHookFailed`.
    Post,
    /// Any other hook (not run by the operations of this crate).
    Other,
}

/// Classify a hook by name (§1.11 table).
pub(crate) fn hook_class(name: &str) -> HookClass {
    match name {
        "pre-commit"
        | "prepare-commit-msg"
        | "commit-msg"
        | "pre-merge-commit"
        | "pre-push"
        | "pre-rebase"
        | "pre-applypatch"
        | "applypatch-msg"
        | "pre-auto-gc"
        | "reference-transaction" => HookClass::Pre,
        "post-checkout" | "post-merge" | "post-commit" | "post-rewrite" | "post-applypatch"
        | "post-index-change" => HookClass::Post,
        _ => HookClass::Other,
    }
}

/// One process's trace file, reduced to what attribution needs.
#[derive(Default)]
struct ProcessTrace {
    sid: String,
    /// `child_id` -> (class, hook name).
    started: HashMap<u64, (String, Option<String>)>,
    /// `child_id` -> (pid, exit code, duration in ms), in order of exit.
    exited: Vec<(u64, Option<u32>, Option<i32>, u64)>,
    /// Hook `child_id`s in start order.
    hook_order: Vec<u64>,
}

impl ProcessTrace {
    fn parse(text: &str) -> Option<ProcessTrace> {
        let mut trace = ProcessTrace::default();
        for line in text.lines() {
            // Only `child_start` / `child_exit` matter; a long rebase writes thousands of region and data events.
            if !line.contains("\"child_") {
                continue;
            }
            let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let Some(sid) = event.get("sid").and_then(|v| v.as_str()) else {
                continue;
            };
            if trace.sid.is_empty() {
                trace.sid = sid.to_owned();
            } else if trace.sid != sid {
                // One file per process: a different sid means the file is not what we expect.
                return None;
            }
            let child_id = || event.get("child_id").and_then(|v| v.as_u64());
            match event.get("event").and_then(|v| v.as_str()) {
                Some("child_start") => {
                    let Some(id) = child_id() else { continue };
                    let class = event
                        .get("child_class")
                        .and_then(|v| v.as_str())
                        .unwrap_or("?")
                        .to_owned();
                    let hook_name = event
                        .get("hook_name")
                        .and_then(|v| v.as_str())
                        .map(str::to_owned);
                    if class == "hook" {
                        trace.hook_order.push(id);
                    }
                    trace.started.insert(id, (class, hook_name));
                }
                Some("child_exit") => {
                    let Some(id) = child_id() else { continue };
                    let pid = event
                        .get("pid")
                        .and_then(|v| v.as_u64())
                        .and_then(|p| u32::try_from(p).ok());
                    let code = event
                        .get("code")
                        .and_then(|v| v.as_i64())
                        .and_then(|c| i32::try_from(c).ok());
                    let duration_ms = event
                        .get("t_rel")
                        .and_then(|v| v.as_f64())
                        .map(|secs| (secs * 1000.0).round().max(0.0) as u64)
                        .unwrap_or(0);
                    trace.exited.push((id, pid, code, duration_ms));
                }
                _ => {}
            }
        }
        (!trace.sid.is_empty()).then_some(trace)
    }

    /// The pids of git children this process started itself (not hooks).
    fn non_hook_child_pids(&self) -> impl Iterator<Item = u32> + '_ {
        self.exited.iter().filter_map(|(id, pid, ..)| {
            let (class, _) = self.started.get(id)?;
            (class != "hook").then_some(*pid).flatten()
        })
    }

    /// The hooks this process ran, in start order.
    fn hooks(&self) -> impl Iterator<Item = HookRun> + '_ {
        self.hook_order.iter().filter_map(|id| {
            let (_, name) = self.started.get(id)?;
            let exit = self.exited.iter().find(|(exit_id, ..)| exit_id == id);
            Some(HookRun {
                name: name.clone().unwrap_or_else(|| "unknown hook".to_owned()),
                // No exit event: the hook (or Git) was killed before it was recorded.
                exit_code: exit.and_then(|(_, _, code, _)| *code),
                duration_ms: exit.map(|(.., ms)| *ms).unwrap_or(0),
            })
        })
    }
}

/// The pid at the end of a sid component (`...-P00006c58`).
fn pid_of_sid_component(component: &str) -> Option<u32> {
    let (_, hex) = component.rsplit_once("-P")?;
    u32::from_str_radix(hex, 16).ok()
}

/// Read every trace file in `dir` and return the hooks run by the command (see the module docs for which processes
/// count). Hooks of the top-level process come first, then those of its git sub-commands in the order the
/// sub-commands' files were attributed.
pub(crate) fn collect(dir: &Path) -> Vec<HookRun> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut traces: Vec<ProcessTrace> = entries
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            let text = std::fs::read(&path).ok()?;
            ProcessTrace::parse(&String::from_utf8_lossy(&text))
        })
        .collect();
    // Deterministic order: the sid starts with a UTC timestamp.
    traces.sort_by(|a, b| a.sid.cmp(&b.sid));

    let mut accepted: HashSet<&str> = HashSet::new();
    let mut ordered: Vec<&ProcessTrace> = Vec::new();
    for trace in &traces {
        if !trace.sid.contains('/') && accepted.insert(&trace.sid) {
            ordered.push(trace);
        }
    }
    // Descend one generation at a time; the depth is tiny (pull -> merge).
    let mut frontier = 0;
    while frontier < ordered.len() {
        let parent = ordered[frontier];
        frontier += 1;
        let child_pids: HashSet<u32> = parent.non_hook_child_pids().collect();
        if child_pids.is_empty() {
            continue;
        }
        for trace in &traces {
            let Some((parent_sid, own)) = trace.sid.rsplit_once('/') else {
                continue;
            };
            if parent_sid == parent.sid
                && pid_of_sid_component(own).is_some_and(|pid| child_pids.contains(&pid))
                && accepted.insert(&trace.sid)
            {
                ordered.push(trace);
            }
        }
    }
    ordered.iter().flat_map(|trace| trace.hooks()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOP: &str = "20261112T012541.130709Z-H63448d6b-P00006c58";

    fn line(sid: &str, rest: &str) -> String {
        format!(r#"{{"event":{rest},"sid":"{sid}","thread":"main"}}"#)
    }

    fn child_start(sid: &str, id: u64, class: &str, hook: Option<&str>) -> String {
        let hook = hook
            .map(|h| format!(r#","hook_name":"{h}""#))
            .unwrap_or_default();
        line(
            sid,
            &format!(r#""child_start","child_id":{id},"child_class":"{class}"{hook}"#),
        )
    }

    fn child_exit(sid: &str, id: u64, pid: u32, code: i32) -> String {
        line(
            sid,
            &format!(r#""child_exit","child_id":{id},"pid":{pid},"code":{code},"t_rel":0.25"#),
        )
    }

    fn write(dir: &Path, sid: &str, lines: &[String]) {
        let name = sid.rsplit('/').next().expect("non-empty sid");
        std::fs::write(dir.join(name), lines.join("\n")).expect("trace file can be written");
    }

    #[test]
    fn top_level_hooks_are_reported_and_nested_hook_gits_ignored() {
        let dir = tempfile::tempdir().expect("temp dir");
        write(
            dir.path(),
            TOP,
            &[
                line(TOP, r#""version","evt":"4""#),
                child_start(TOP, 0, "hook", Some("pre-commit")),
                child_exit(TOP, 0, 39680, 3),
            ],
        );
        // A `git push` started by the pre-commit hook ran its own failing pre-push hook.
        let nested = format!("{TOP}/20261112T012541.606633Z-H63448d6b-P00007e08");
        write(
            dir.path(),
            &nested,
            &[
                child_start(&nested, 0, "hook", Some("pre-push")),
                child_exit(&nested, 0, 1, 9),
            ],
        );
        std::fs::write(dir.path().join("garbage"), b"not json\n{").expect("garbage file");

        assert_eq!(
            collect(dir.path()),
            [HookRun {
                name: "pre-commit".into(),
                exit_code: Some(3),
                duration_ms: 250
            }],
            "only the top-level process counts; the nested git's parent was the hook's shell"
        );
    }

    #[test]
    fn hooks_of_git_subcommands_are_attributed() {
        let dir = tempfile::tempdir().expect("temp dir");
        write(
            dir.path(),
            TOP,
            &[
                child_start(TOP, 0, "?", None),
                child_exit(TOP, 0, 0x7e08, 0),
                child_start(TOP, 1, "hook", Some("fake-hook")),
                child_exit(TOP, 1, 0x1514, 0),
            ],
        );
        let merge = format!("{TOP}/20261112T012623.564658Z-H63448d6b-P00007e08");
        write(
            dir.path(),
            &merge,
            &[
                child_start(&merge, 0, "hook", Some("post-merge")),
                child_exit(&merge, 0, 14800, 5),
            ],
        );
        // A git whose pid is that of the hook child (`exec git ...` inside a hook) does not count.
        let exec_from_hook = format!("{TOP}/20261112T012623.600000Z-H63448d6b-P00001514");
        write(
            dir.path(),
            &exec_from_hook,
            &[
                child_start(&exec_from_hook, 0, "hook", Some("pre-push")),
                child_exit(&exec_from_hook, 0, 2, 1),
            ],
        );

        let names: Vec<_> = collect(dir.path())
            .into_iter()
            .map(|h| (h.name, h.exit_code))
            .collect();
        assert_eq!(
            names,
            [
                ("fake-hook".to_owned(), Some(0)),
                ("post-merge".to_owned(), Some(5))
            ],
            "`git pull` runs `git merge` as its own child, whose post-merge hook belongs to the command"
        );
    }

    #[test]
    fn hook_without_exit_event_has_no_exit_code() {
        let dir = tempfile::tempdir().expect("temp dir");
        write(
            dir.path(),
            TOP,
            &[child_start(TOP, 0, "hook", Some("pre-commit"))],
        );
        assert_eq!(
            collect(dir.path()),
            [HookRun {
                name: "pre-commit".into(),
                exit_code: None,
                duration_ms: 0
            }],
            "a hook that never exited is reported without exit code"
        );
    }

    #[test]
    fn missing_directory_yields_nothing() {
        assert!(
            collect(Path::new("this/does/not/exist")).is_empty(),
            "trace problems never fail"
        );
    }

    #[test]
    fn hook_classes() {
        assert_eq!(hook_class("pre-push"), HookClass::Pre, "pre hook");
        assert_eq!(
            hook_class("reference-transaction"),
            HookClass::Pre,
            "pre hook"
        );
        assert_eq!(hook_class("post-checkout"), HookClass::Post, "post hook");
        assert_eq!(
            hook_class("post-index-change"),
            HookClass::Post,
            "post hook"
        );
        assert_eq!(hook_class("update"), HookClass::Other, "server-side hook");
    }

    #[test]
    fn pid_from_sid() {
        assert_eq!(
            pid_of_sid_component("20261112T012541.606633Z-H63448d6b-P00007e08"),
            Some(0x7e08),
            "hex pid at the end"
        );
        assert_eq!(pid_of_sid_component("garbage"), None, "no pid");
    }
}
