//! The verb table of the scenario language, and the one registration function through which every region adds its
//! own verbs.
//!
//! # Adding a verb (for region packages)
//!
//! In the region's `register(cx)` (or any code that runs at startup with an `&mut App`):
//!
//! ```ignore
//! #[cfg(feature = "qa")]
//! crate::qa::register_verb(
//!     cx,
//!     crate::qa::Verb::new(
//!         "search log",                       // the words that select the verb (longest match wins)
//!         "search log <text>",                // shown in `verbs.txt`
//!         "Types <text> into the log search", // one line of help
//!         |window, cx, args| {
//!             let text = args.rest(0);        // everything after the verb words, `$SANDBOX`/`$REPOS` expanded
//!             /* call the region's public entry points on the UI thread */
//!             Ok(format!("searched for {text}")) // `Err(message)` fails the step
//!         },
//!     ),
//! );
//! ```
//!
//! A handler runs synchronously on the UI thread with the window and the app; it must call the app's own actions or
//! public entry points (never OS-level input). After a step that returned `Ok` the runner waits until the app is
//! idle again (operations finished, loads settled, frames drawn), so the next step sees the effect. A region whose
//! own asynchronous work is invisible to the runner adds an idle probe with [`crate::qa::register_idle_probe`].
//! Register a verb under an existing name to replace it. Every registered verb is listed in `verbs.txt` of the
//! output directory.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::str::FromStr;

use gpui_kit::{App, Global, Window};

use super::dsl;

/// The outcome of a step: `Ok(detail)` passes (the detail goes into the report), `Err(message)` fails.
pub type StepResult = Result<String, String>;

/// A synchronous verb handler.
pub type Handler = Rc<dyn Fn(&mut Window, &mut App, &Args) -> StepResult>;

/// Verbs the runner executes itself because they wait or end the run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunnerVerb {
    /// `wait idle [ms]`.
    WaitIdle,
    /// `wait <ms>`.
    Wait,
    /// `shot <name> [window]`.
    Shot,
    /// `quit [code]`.
    Quit,
}

#[derive(Clone)]
pub(crate) enum Action {
    Sync(Handler),
    Runner(RunnerVerb),
}

/// One verb of the scenario language.
#[derive(Clone)]
pub struct Verb {
    pub(crate) name: &'static str,
    pub(crate) usage: &'static str,
    pub(crate) help: &'static str,
    pub(crate) action: Action,
    pub(crate) settle: bool,
}

impl Verb {
    /// A verb that runs `handler` on the UI thread. `name` is the space-separated words that select it.
    pub fn new(
        name: &'static str,
        usage: &'static str,
        help: &'static str,
        handler: impl Fn(&mut Window, &mut App, &Args) -> StepResult + 'static,
    ) -> Self {
        Self {
            name,
            usage,
            help,
            action: Action::Sync(Rc::new(handler)),
            settle: true,
        }
    }

    pub(crate) fn runner(
        name: &'static str,
        usage: &'static str,
        help: &'static str,
        kind: RunnerVerb,
    ) -> Self {
        Self {
            name,
            usage,
            help,
            action: Action::Runner(kind),
            settle: false,
        }
    }

    /// The runner does not wait for idle after this verb (assertions: they must look at the state as it is).
    pub fn without_settle(mut self) -> Self {
        self.settle = false;
        self
    }

    /// The words that select the verb.
    pub fn name(&self) -> &'static str {
        self.name
    }

    fn words(&self) -> impl Iterator<Item = &'static str> {
        self.name.split(' ')
    }
}

/// The registered verbs (an app global).
#[derive(Default)]
pub struct VerbTable {
    verbs: Vec<Verb>,
}

impl Global for VerbTable {}

impl VerbTable {
    fn add(&mut self, verb: Verb) {
        match self.verbs.iter_mut().find(|v| v.name == verb.name) {
            Some(existing) => *existing = verb,
            None => self.verbs.push(verb),
        }
    }
}

/// Registers `verb`; a verb with the same name is replaced. See the module documentation.
pub fn register_verb(cx: &mut App, verb: Verb) {
    cx.default_global::<VerbTable>().add(verb);
}

/// The verb a line starts with (the longest matching name) and the number of words its name consumed.
pub(crate) fn lookup(cx: &App, tokens: &[String]) -> Option<(Verb, usize)> {
    let table = cx.try_global::<VerbTable>()?;
    table
        .verbs
        .iter()
        .filter_map(|verb| {
            let words = verb.words().count();
            let matches = tokens.len() >= words
                && verb
                    .words()
                    .zip(tokens)
                    .all(|(word, token)| word.eq_ignore_ascii_case(token));
            matches.then(|| (verb.clone(), words))
        })
        .max_by_key(|(_, words)| *words)
}

/// `verbs.txt`: one line per verb, sorted, `usage - help`.
pub fn describe(cx: &App) -> String {
    let mut lines: Vec<String> = cx
        .try_global::<VerbTable>()
        .map(|t| {
            t.verbs
                .iter()
                .map(|v| format!("{} - {}", v.usage, v.help))
                .collect()
        })
        .unwrap_or_default();
    lines.sort();
    lines.join("\n") + "\n"
}

/// The arguments of a step: the words after the verb's own words.
#[derive(Clone, Debug)]
pub struct Args {
    tokens: Vec<String>,
    sandbox: PathBuf,
}

impl Args {
    /// Arguments `tokens` of a step run in `sandbox`.
    pub fn new(tokens: Vec<String>, sandbox: PathBuf) -> Self {
        Self { tokens, sandbox }
    }

    /// Number of arguments.
    pub fn len(&self) -> usize {
        self.tokens.len()
    }

    /// No arguments.
    pub fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    /// The sandbox directory.
    pub fn sandbox(&self) -> &Path {
        &self.sandbox
    }

    /// Argument `ix` with `$SANDBOX` / `$REPOS` expanded, or an error naming `what`.
    pub fn text(&self, ix: usize, what: &str) -> Result<String, String> {
        self.tokens
            .get(ix)
            .map(|t| dsl::expand(t, &self.sandbox))
            .ok_or_else(|| format!("missing argument: {what}"))
    }

    /// Argument `ix` if present.
    pub fn opt(&self, ix: usize) -> Option<String> {
        self.tokens.get(ix).map(|t| dsl::expand(t, &self.sandbox))
    }

    /// Argument `ix` parsed as a number.
    pub fn number<T: FromStr>(&self, ix: usize, what: &str) -> Result<T, String> {
        let text = self.text(ix, what)?;
        text.parse()
            .map_err(|_| format!("{what} must be a number, got '{text}'"))
    }

    /// Arguments `from..` joined with spaces (empty when there are none).
    pub fn rest(&self, from: usize) -> String {
        self.tokens
            .iter()
            .skip(from)
            .map(|t| dsl::expand(t, &self.sandbox))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Whether any argument from `from` on equals `word` (case-insensitive): trailing flags such as `window`.
    pub fn has_flag(&self, from: usize, word: &str) -> bool {
        self.tokens
            .iter()
            .skip(from)
            .any(|t| t.eq_ignore_ascii_case(word))
    }

    /// The raw words (no expansion).
    pub fn raw(&self) -> &[String] {
        &self.tokens
    }
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use super::*;

    fn args(words: &[&str]) -> Args {
        Args::new(
            words.iter().map(|w| (*w).to_owned()).collect(),
            PathBuf::from("/sbx"),
        )
    }

    #[test]
    fn typed_accessors_report_what_is_wrong() {
        let a = args(&["12", "x"]);
        assert_eq!(a.number::<usize>(0, "row"), Ok(12), "a number parses");
        assert!(
            a.number::<usize>(1, "row")
                .expect_err("not a number")
                .contains("row"),
            "the error names the argument"
        );
        assert!(
            a.text(2, "path").expect_err("missing").contains("path"),
            "a missing argument is named"
        );
        assert_eq!(a.rest(1), "x", "rest joins from the index");
        assert!(a.has_flag(0, "X"), "flags compare case-insensitively");
    }
}
