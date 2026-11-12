//! QA scenario verbs of the AI feature (feature `qa`; spec 21 §8.4; `tools/qa/scenarios/m2-ai.qa`).
//!
//! A QA run starts no services and never touches the network or the keychain. These verbs bring up what the
//! feature needs, entirely inside the process: the real services on the sandbox's data directory, an in-memory key
//! store and a fake OpenAI-compatible provider on a loopback port that streams a canned commit message.

use std::time::Duration;

use gpui_kit::{App, Global, Window};

use super::controller::{self, AiController};
use super::fake::{Behavior, FakeProvider};
use super::hub::{self, KeyStoreGlobal, Readiness};
use crate::app;
use crate::app_view::AppViewGlobal;
use crate::backend::ai::{self as backend_ai, AiProvider, AiSettings, KeyStore};
use crate::qa::{self, Args, StepResult, Verb, monitor, register_idle_probe, register_verb};
use crate::settings;
use crate::tab::TabCtx;

/// The commit message the fake provider streams, word by word.
const CANNED_MESSAGE: &str = "feat: validate the sign-up form before it is sent\n\n\
Reject empty names and malformed addresses in the browser so that users see\n\
the problem next to the field instead of after a round trip.\n\n\
- add a validator for the name and the address fields\n\
- show the first error under the field that caused it";

/// The key every hosted-provider scenario stores in the in-memory key store (not a real key).
const PLACEHOLDER_KEY: &str = "sk-qa-placeholder-not-a-real-key";

/// The running fake provider of the run.
struct QaProvider(#[allow(dead_code)] FakeProvider);

impl Global for QaProvider {}

/// Set while the services are being started.
struct ServicesStarting;

impl Global for ServicesStarting {}

/// Registers the verbs and the idle probe.
pub fn register(cx: &mut App) {
    register_verb(
        cx,
        Verb::new(
            "ai use fake provider",
            "ai use fake provider [delay-ms]",
            "Starts the services and a fake local provider that streams a canned commit message (one word per delay-ms, default 120) and selects it in the AI settings",
            |_, cx, args| use_fake_provider(cx, args),
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "ai use hosted provider",
            "ai use hosted provider",
            "Selects OpenAI with a placeholder key in the in-memory key store (nothing is sent unless a privacy confirmation is accepted)",
            |_, cx, _| use_hosted_provider(cx),
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "ai reset",
            "ai reset",
            "Selects no provider, forgets the privacy confirmation and empties the in-memory key store",
            |_, cx, _| reset(cx),
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "ai prepare staged changes",
            "ai prepare staged changes",
            "Edits and adds files of the selected repository (inside the sandbox) and stages everything",
            prepare_staged_changes,
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "ai set message",
            "ai set message <text>",
            "Replaces the commit message",
            |window, cx, args| {
                let text = args.rest(0);
                with_commit_box(cx, |commit_box, cx| {
                    commit_box.update(cx, |b, cx| b.set_message(text.clone(), window, cx));
                })?;
                Ok(text)
            },
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "ai generate",
            "ai generate",
            "Clicks Generate: streams a commit message into the box (does not wait for it to finish: use `wait idle`)",
            generate,
        )
        .without_settle(),
    );
    register_verb(
        cx,
        Verb::new(
            "ai stop",
            "ai stop",
            "Clicks Stop while a message streams",
            |_, cx, _| {
                let controller = active_controller(cx)?;
                let stopped = controller.update(cx, |c, cx| c.stop(cx));
                Ok(if stopped {
                    "stopped"
                } else {
                    "nothing was running"
                }
                .to_owned())
            },
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "ai test connection",
            "ai test connection",
            "Clicks \"Test connection\" on the AI settings page (refused for a provider off this machine: a QA run never contacts a real AI service)",
            |window, cx, _| {
                if let Some(refusal) = refuse_hosted(cx) {
                    return Err(refusal);
                }
                let page = crate::dialogs::settings::ai::page_entity(window, cx);
                page.update(cx, |page, cx| page.test_connection(cx));
                Ok(String::new())
            },
        ),
    );
    register_idle_probe(cx, probe);
}

/// Why the QA run refuses a request to the selected provider, or `None`. A QA run never contacts a real AI
/// service: a provider off this machine is refused even after the privacy confirmation was accepted (the fake
/// provider of `ai use fake provider` is on the loopback interface and passes).
pub(crate) fn refuse_hosted(cx: &App) -> Option<String> {
    qa::session(cx)?;
    let ai = backend_ai::parse_settings(&settings::get_or_default(cx).ai);
    let (provider, host) = backend_ai::hosted_target(&ai)?;
    Some(format!(
        "refusing to contact {provider} at {host}: QA uses `ai use fake provider` for traffic"
    ))
}

fn start_failed(what: &str, err: impl std::fmt::Display) -> String {
    format!("{what}: {err}")
}

/// The in-memory key store, installed once (a QA run never reads the keychain).
fn in_memory_keys(cx: &mut App) -> KeyStore {
    if !cx.has_global::<QaKeys>() {
        cx.set_global(KeyStoreGlobal(KeyStore::in_memory().0));
        cx.set_global(QaKeys);
    }
    hub::key_store(cx)
}

/// Marks the installed key store as the QA one.
struct QaKeys;

impl Global for QaKeys {}

/// Starts the services in the background unless they run or are starting.
fn ensure_services(cx: &mut App) {
    if app::services(cx).is_some() || cx.has_global::<ServicesStarting>() {
        return;
    }
    cx.set_global(ServicesStarting);
    let current = settings::get_or_default(cx);
    let task = crate::backend::bg(cx, move || {
        crate::backend::services::start_services(&current)
    });
    cx.spawn(async move |cx| {
        let result = task.await;
        cx.update(|cx| {
            cx.remove_global::<ServicesStarting>();
            match result {
                Ok(handle) => {
                    if let Some(state) = app::state_mut(cx) {
                        state.services = Some(handle);
                    }
                    hub::keys_changed(cx);
                    cx.refresh_windows();
                }
                Err(err) => tracing::error!("QA: the services did not start: {err:#}"),
            }
        });
    })
    .detach();
}

fn use_fake_provider(cx: &mut App, args: &Args) -> StepResult {
    let delay = args.opt(0).map_or(Ok(120), |text| {
        text.parse::<u64>()
            .map_err(|_| format!("delay-ms must be a number, got '{text}'"))
    })?;
    in_memory_keys(cx);
    let pieces: Vec<String> = CANNED_MESSAGE
        .split_inclusive(' ')
        .flat_map(|word| word.split_inclusive('\n'))
        .map(str::to_owned)
        .collect();
    let fake = FakeProvider::start(Behavior::Reply {
        pieces,
        delay: Duration::from_millis(delay),
    })
    .map_err(|err| start_failed("cannot start the fake provider", err))?;
    let mut ai = AiSettings {
        provider: Some(AiProvider::LMStudio),
        ..AiSettings::default()
    };
    ai.lmstudio.endpoint = fake.endpoint();
    ai.lmstudio.model = "qa-fake-model".into();
    let endpoint = fake.endpoint();
    cx.set_global(QaProvider(fake));
    let value = backend_ai::settings_value(&ai);
    settings::update(cx, |s| {
        s.ai = value;
        s.ai_hosted_consent = false;
    });
    ensure_services(cx);
    hub::keys_changed(cx);
    Ok(endpoint)
}

fn use_hosted_provider(cx: &mut App) -> StepResult {
    let keys = in_memory_keys(cx);
    keys.store("aiOpenAIKey", PLACEHOLDER_KEY)
        .map_err(|err| start_failed("cannot store the placeholder key", err))?;
    let ai = AiSettings {
        provider: Some(AiProvider::OpenAi),
        ..AiSettings::default()
    };
    let value = backend_ai::settings_value(&ai);
    settings::update(cx, |s| {
        s.ai = value;
        s.ai_hosted_consent = false;
    });
    ensure_services(cx);
    hub::keys_changed(cx);
    Ok("OpenAI".to_owned())
}

fn reset(cx: &mut App) -> StepResult {
    let keys = in_memory_keys(cx);
    for provider in backend_ai::PROVIDERS {
        if let Some(handle) = AiSettings::secret_handle(provider) {
            keys.delete(handle)
                .map_err(|err| start_failed("cannot empty the key store", err))?;
        }
    }
    settings::update(cx, |s| {
        s.ai = serde_json::Value::Null;
        s.ai_hosted_consent = false;
    });
    hub::keys_changed(cx);
    Ok(String::new())
}

fn active_ctx(cx: &App) -> Result<TabCtx, String> {
    cx.try_global::<AppViewGlobal>()
        .and_then(|g| g.0.upgrade())
        .and_then(|view| {
            view.read(cx)
                .active_tab()
                .map(|tab| tab.read(cx).ctx().clone())
        })
        .ok_or_else(|| "the selected tab is not a repository (open one first)".to_owned())
}

fn active_controller(cx: &App) -> Result<gpui_kit::Entity<AiController>, String> {
    let ctx = active_ctx(cx)?;
    controller::of(ctx.id, cx).ok_or_else(|| "the tab has no AI controller".to_owned())
}

fn with_commit_box<R>(
    cx: &mut App,
    f: impl FnOnce(gpui_kit::Entity<crate::status_pane::commit_box::CommitBox>, &mut App) -> R,
) -> Result<R, String> {
    let ctx = active_ctx(cx)?;
    let tab = ctx.tab.upgrade().ok_or("the tab is gone")?;
    let commit_box = tab.read(cx).views().status.read(cx).commit_box().clone();
    Ok(f(commit_box, cx))
}

fn generate(window: &mut Window, cx: &mut App, _: &Args) -> StepResult {
    if super::jobs::needs_consent(cx) {
        monitor::scenario_dialog(cx, true);
    } else if let Some(refusal) = refuse_hosted(cx) {
        // Past the privacy confirmation the request would really leave the machine.
        return Err(refusal);
    }
    let controller = active_controller(cx)?;
    let commit_box = with_commit_box(cx, |b, _| b.downgrade())?;
    controller.update(cx, |c, cx| c.generate(commit_box, window, cx));
    Ok(String::new())
}

fn prepare_staged_changes(window: &mut Window, cx: &mut App, _: &Args) -> StepResult {
    let ctx = active_ctx(cx)?;
    let workdir = ctx
        .backend
        .handle()
        .and_then(|handle| handle.workdir())
        .ok_or("the selected tab has no working directory")?;
    qa::session(cx)
        .ok_or("no QA session")?
        .sandbox
        .check_repository(&workdir)?;
    let edit = |name: &str, f: &dyn Fn(String) -> String| -> Result<(), String> {
        let path = workdir.join(name);
        let old = std::fs::read_to_string(&path).unwrap_or_default();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|err| format!("{}: {err}", dir.display()))?;
        }
        std::fs::write(&path, f(old)).map_err(|err| format!("{}: {err}", path.display()))
    };
    edit("src/main.txt", &|old| {
        format!("{old}    let value_31 = validate(input.name, input.address);\n")
    })?;
    edit("src/validate.txt", &|_| {
        "fn validate(name: &str, address: &str) -> Result<(), FormError> {\n    require(name)?;\n    require_address(address)\n}\n"
            .to_owned()
    })?;
    ctx.with_tab(window, cx, |tab, window, cx| {
        tab.reload(crate::backend::RefreshSet::STATUS, cx);
        tab.stage_all(window, cx);
    });
    Ok(workdir.display().to_string())
}

/// Busy while the services start, while the settings reach the service, and while a message streams.
fn probe(_: &mut Window, cx: &App) -> Option<String> {
    if cx.has_global::<ServicesStarting>() {
        return Some("the AI services are starting".into());
    }
    if app::services(cx).is_some() && matches!(hub::readiness(cx), Readiness::Pending) {
        return Some("the AI settings are being applied".into());
    }
    let controller = active_controller(cx).ok()?;
    controller
        .read(cx)
        .is_generating()
        .then(|| "a commit message is being generated".to_owned())
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use gpui_kit::test::TestWindowExt as _;
    use gpui_kit::{AnyWindowHandle, AppContext as _, TestAppContext};

    use crate::qa::args::QaArgs;
    use crate::qa::gate::Sandbox;
    use crate::qa::verbs::{self, Action};
    use crate::qa::{Args, QaRun, dsl, init};
    use crate::test_support::{DEFAULT_SIZE, Fixture, mount_fixture, toast_texts};

    fn run_line(
        cx: &mut TestAppContext,
        handle: AnyWindowHandle,
        sandbox: &std::path::Path,
        text: &str,
    ) -> Result<String, String> {
        let lines = dsl::parse(text).expect("a valid line");
        let sandbox = sandbox.to_path_buf();
        cx.update_window(handle, |_, window, cx| {
            let (verb, consumed) = verbs::lookup(cx, &lines[0].tokens)
                .unwrap_or_else(|| panic!("no verb for '{text}'"));
            let args = Args::new(lines[0].tokens[consumed..].to_vec(), sandbox);
            match &verb.action {
                Action::Sync(handler) => handler(window, cx, &args),
                Action::Runner(kind) => panic!("{kind:?} is a runner verb"),
            }
        })
        .expect("the test window is open")
    }

    /// Waits (in real time: the services start on a background thread) until `done` holds.
    fn wait_until(cx: &mut TestAppContext, what: &str, done: impl Fn(&mut TestAppContext) -> bool) {
        for _ in 0..200 {
            cx.run_until_parked();
            if done(cx) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        panic!("timed out waiting for {what}");
    }

    fn ready(cx: &mut TestAppContext) -> bool {
        matches!(
            cx.update(|cx| super::hub::readiness(cx)),
            super::Readiness::Ready { .. }
        )
    }

    /// The verbs of `tools/qa/scenarios/m2-ai.qa`, run headless against a real repository.
    #[gpui_kit::test]
    fn the_ai_scenario_verbs_work_end_to_end(cx: &mut TestAppContext) {
        let fx = Fixture::new();
        fx.write(
            "README.md",
            "# demo
",
        );
        fx.commit_all("init");
        let (handle, app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
        let sandbox = fx.dir.path().to_path_buf();
        let run = QaRun {
            sandbox: Sandbox::for_tests(sandbox.clone()),
            args: QaArgs::default(),
        };
        cx.update(|cx| init(cx, &run));
        let listing = cx.update(|cx| verbs::describe(cx));
        for verb in [
            "ai use fake provider",
            "ai use hosted provider",
            "ai reset",
            "ai prepare staged changes",
            "ai generate",
            "ai stop",
            "ai test connection",
            "ai set message",
        ] {
            assert!(listing.contains(verb), "{verb} is listed in verbs.txt");
        }
        let line = |cx: &mut TestAppContext, text: &str| {
            run_line(cx, handle, &sandbox, text).unwrap_or_else(|err| panic!("{text}: {err}"))
        };

        let endpoint = line(cx, "ai use fake provider 0");
        assert!(endpoint.starts_with("http://127.0.0.1:"), "{endpoint}");
        wait_until(cx, "the fake provider to be ready", ready);

        line(cx, "ai prepare staged changes");
        let staged = |cx: &mut TestAppContext| {
            cx.update(|cx| {
                tab.read(cx)
                    .ctx()
                    .model
                    .read(cx)
                    .status
                    .value
                    .as_ref()
                    .map_or(0, |s| s.staged.len())
            })
        };
        wait_until(cx, "the changes to be staged", |cx| staged(cx) == 2);

        line(cx, "ai set message wip: sign-up form");
        line(cx, "ai generate");
        wait_until(cx, "the message", |cx| {
            let b = cx.update(|cx| tab.read(cx).views().status.read(cx).commit_box().clone());
            cx.update(|cx| b.read(cx).message(cx))
                .starts_with("feat: validate the sign-up form")
        });

        line(cx, "ai test connection");
        wait_until(cx, "the connection test", |cx| {
            let page = cx
                .update_window(handle, |_, window, cx| {
                    crate::dialogs::settings::ai::page_entity(window, cx)
                })
                .expect("window");
            cx.update(|cx| {
                *page.read(cx).test_state() == crate::dialogs::settings::ai::TestState::Ok
            })
        });

        line(cx, "ai use hosted provider");
        wait_until(cx, "OpenAI to be ready", ready);
        line(cx, "ai set message");
        line(cx, "ai generate");
        cx.run_until_parked();
        let consent = cx
            .update_window(handle, |_, window, cx| {
                window.render_frame(cx);
                window.try_find("dialog-primary").is_some()
            })
            .expect("window");
        assert!(consent, "a hosted provider asks before anything is sent");
        assert!(
            !cx.update(|cx| toast_texts(&app, cx))
                .iter()
                .any(|t| t.contains("Couldn't")),
            "no error"
        );
        // Even an accepted confirmation sends nothing to the real service in a QA run.
        cx.update_window(handle, |_, window, cx| {
            window.render_frame(cx);
            window.click("dialog-primary", cx);
        })
        .expect("window");
        cx.run_until_parked();
        assert!(
            cx.update(|cx| toast_texts(&app, cx))
                .iter()
                .any(|t| t.contains("refusing to contact OpenAI")),
            "the request was refused"
        );
        let controller = cx.update(|cx| tab.read(cx).views().ai.clone());
        assert!(!cx.update(|cx| controller.read(cx).is_generating()));
        assert!(
            run_line(cx, handle, &sandbox, "ai test connection").is_err(),
            "no connection test against OpenAI"
        );
        let page = cx
            .update_window(handle, |_, window, cx| {
                crate::dialogs::settings::ai::page_entity(window, cx)
            })
            .expect("window");
        cx.update(|cx| page.update(cx, |page, cx| page.test_connection(cx)));
        assert!(
            matches!(
                cx.update(|cx| page.read(cx).test_state().clone()),
                crate::dialogs::settings::ai::TestState::Failed(reason) if reason.contains("refusing")
            ),
            "the page's Test connection is refused too"
        );
        let suggested = std::rc::Rc::new(std::cell::RefCell::new(None));
        let sink = suggested.clone();
        cx.update_window(handle, |_, window, cx| {
            super::super::suggest_branch_name(
                window,
                cx,
                super::super::BranchNameSource::CommitMessage("x".into()),
                move |result, _, _| *sink.borrow_mut() = Some(result),
            );
        })
        .expect("window");
        cx.run_until_parked();
        assert!(
            matches!(&*suggested.borrow(), Some(Err(crate::backend::ai::AiFailure::Failed(reason))) if reason.contains("refusing")),
            "the hooks are refused too"
        );
        line(cx, "ai reset");
        cx.run_until_parked();
    }
}
