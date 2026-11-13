//! QA verbs of the forge integration (feature `qa`): they run the REAL code paths (forge detection, refresh,
//! badges, the Accounts page, the create-PR dialog) against a fake GitHub Enterprise server on loopback and an
//! in-memory keychain. Nothing leaves the machine and no real keychain entry is written.
//!
//! ```text
//! forge mock start [<branch>=<open|draft>[:<passed|failed|running>]]...   fake forge + services + origin URL
//! forge mock sign in                                                      connects the fake account
//! forge mock merge <branch>   forge mock pr <branch> <open|draft> [ci]    change the fake forge
//! forge mock offline on|off   forge refresh
//! open forge dialog github-token|enterprise|gitlab-token|gitlab-selfhosted|bitbucket|device
//! assert forge badge <branch> <open|draft|merged|closed|none> [passed|failed|running|none]
//! assert forge status <idle|no-account|offline|...>
//! ```

use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::{App, Entity, Global, Window};

use super::ForgeController;
use crate::app_view::AppViewGlobal;
use crate::backend::Sensitive;
use crate::backend::forge::testing::{
    FakeCheck, FakeForge, failing, passing, running, services_with,
};
use crate::backend::forge::{
    DeviceCodeInfo, DevicePoll, Failure, NewAccount, PrState, add_account,
};
use crate::forge::flows::{DeviceFlowBackend, TokenFlow, open_device_dialog, open_token_dialog};
use crate::qa::{Args, StepResult, Verb, register_idle_probe, register_verb};
use crate::tab::TabCtx;

struct QaFake(Rc<FakeForge>);

impl Global for QaFake {}

fn active_ctx(cx: &App) -> Result<TabCtx, String> {
    let view = cx
        .try_global::<AppViewGlobal>()
        .and_then(|g| g.0.upgrade())
        .ok_or_else(|| "the window content is gone".to_owned())?;
    let tab = view
        .read(cx)
        .active_tab()
        .ok_or_else(|| "the selected tab is not a repository (open one first)".to_owned())?;
    Ok(tab.read(cx).ctx().clone())
}

fn controller(cx: &App) -> Result<Entity<ForgeController>, String> {
    let ctx = active_ctx(cx)?;
    super::controller_of(ctx.id, cx).ok_or_else(|| "the tab has no forge controller".to_owned())
}

fn fake(cx: &App) -> Result<Rc<FakeForge>, String> {
    cx.try_global::<QaFake>()
        .map(|g| g.0.clone())
        .ok_or_else(|| "start the fake forge first: forge mock start".to_owned())
}

fn checks_of(word: &str) -> Result<Vec<FakeCheck>, String> {
    match word {
        "passed" => Ok(passing()),
        "failed" => Ok(failing()),
        "running" => Ok(running()),
        "none" => Ok(Vec::new()),
        other => Err(format!(
            "unknown CI state '{other}' (passed, failed, running, none)"
        )),
    }
}

fn tip_of(ctx: &TabCtx, branch: &str, cx: &App) -> Result<String, String> {
    let model = ctx.model.read(cx);
    let refs = model
        .refs
        .value
        .as_ref()
        .ok_or_else(|| "the refs are not loaded".to_owned())?;
    refs.local
        .iter()
        .find(|b| b.name == branch)
        .map(|b| b.target.to_hex().to_string())
        .ok_or_else(|| format!("no local branch '{branch}'"))
}

/// `branch=open:passed` → (branch, draft, checks).
fn parse_pr(word: &str) -> Result<(String, bool, Vec<FakeCheck>), String> {
    let (branch, rest) = word
        .split_once('=')
        .ok_or_else(|| format!("expected <branch>=<open|draft>[:<ci>], got '{word}'"))?;
    let (state, ci) = rest.split_once(':').unwrap_or((rest, "none"));
    let draft = match state {
        "open" => false,
        "draft" => true,
        other => return Err(format!("unknown PR state '{other}' (open, draft)")),
    };
    Ok((branch.to_owned(), draft, checks_of(ci)?))
}

fn mock_start(window: &mut Window, cx: &mut App, args: &Args) -> StepResult {
    let ctx = active_ctx(cx)?;
    let forge = Rc::new(FakeForge::start());
    let mut described = Vec::new();
    for word in args.raw() {
        let (branch, draft, checks) = parse_pr(word)?;
        let sha = tip_of(&ctx, &branch, cx)?;
        let number = forge.add(&branch, &format!("PR for {branch}"), draft, &sha, checks);
        described.push(format!("#{number} {branch}"));
    }
    let data = crate::paths::data_dir();
    let cache = crate::paths::cache_dir();
    let services = services_with(&data, &cache, Some("qa-client-id"));
    let url = forge.server.remote_url();
    if let Some(state) = crate::app::state_mut(cx) {
        state.services = Some(services);
    }
    cx.set_global(QaFake(forge));
    // The repository's origin now points at the fake forge: the edit is the app's own operation.
    ctx.with_tab(window, cx, |tab, window, cx| {
        tab.edit_remote("origin".into(), url, None, window, cx);
    });
    Ok(format!("fake forge started: {}", described.join(", ")))
}

fn mock_sign_in(_window: &mut Window, cx: &mut App, _args: &Args) -> StepResult {
    let forge = fake(cx)?;
    let services = crate::app::services(cx).ok_or_else(|| "no services".to_owned())?;
    let account = add_account(
        &services,
        NewAccount::GitHubEnterprise {
            api_url: forge.server.api_url(),
            token: Sensitive::new("qa-token".to_owned()),
        },
    )
    .map_err(|f| f.message)?;
    if let Some(model) = super::accounts::installed(cx) {
        model.update(cx, |model, cx| model.added(cx));
    }
    Ok(format!(
        "signed in as {} ({})",
        account.name, account.caption
    ))
}

fn mock_merge(_window: &mut Window, cx: &mut App, args: &Args) -> StepResult {
    let branch = args.text(0, "branch")?;
    fake(cx)?.merge(&branch);
    let controller = controller(cx)?;
    controller.update(cx, |c, cx| c.refresh_now(cx));
    Ok(format!("{branch} merged on the fake forge"))
}

fn mock_pr(_window: &mut Window, cx: &mut App, args: &Args) -> StepResult {
    let ctx = active_ctx(cx)?;
    let branch = args.text(0, "branch")?;
    let state = args.text(1, "open|draft")?;
    let (_, draft, checks) = parse_pr(&format!(
        "{branch}={state}:{}",
        args.opt(2).unwrap_or_else(|| "none".into())
    ))?;
    let sha = tip_of(&ctx, &branch, cx)?;
    let number = fake(cx)?.add(&branch, &format!("PR for {branch}"), draft, &sha, checks);
    controller(cx)?.update(cx, |c, cx| c.refresh_now(cx));
    Ok(format!("#{number} {branch}"))
}

fn mock_offline(_window: &mut Window, cx: &mut App, args: &Args) -> StepResult {
    let on = match args.text(0, "on|off")?.as_str() {
        "on" => true,
        "off" => false,
        other => return Err(format!("expected on or off, got '{other}'")),
    };
    fake(cx)?.server.set_offline(on);
    Ok(format!("fake forge offline: {on}"))
}

fn refresh(_window: &mut Window, cx: &mut App, _args: &Args) -> StepResult {
    controller(cx)?.update(cx, |c, cx| c.refresh_now(cx));
    Ok("refresh requested".to_owned())
}

fn open_forge_dialog(window: &mut Window, cx: &mut App, args: &Args) -> StepResult {
    let name = args.text(0, "dialog name")?;
    // The scenario opens these dialogs itself: `assert no error` must not report them.
    crate::qa::monitor::scenario_dialog(cx, true);
    match name.as_str() {
        "github-token" => open_token_dialog(window, cx, TokenFlow::GitHub),
        "enterprise" => open_token_dialog(window, cx, TokenFlow::GitHubEnterprise),
        "gitlab-token" => open_token_dialog(window, cx, TokenFlow::GitLab),
        "gitlab-selfhosted" => open_token_dialog(window, cx, TokenFlow::GitLabSelfHosted),
        "bitbucket" => open_token_dialog(window, cx, TokenFlow::Bitbucket),
        "device" => {
            // GitHub is never asked: the code is made up and the sign-in never completes.
            cx.set_global(DeviceFlowBackend {
                start: Arc::new(|| {
                    Ok(DeviceCodeInfo {
                        user_code: "ABCD-1234".into(),
                        device_code: "qa-device-code".into(),
                        verification_uri: "https://github.com/login/device".into(),
                        interval: 5,
                        expires_in: 900,
                    })
                }),
                poll: Arc::new(|_, _| Ok::<DevicePoll, Failure>(DevicePoll::Pending)),
            });
            open_device_dialog(window, cx);
        }
        other => return Err(format!("unknown forge dialog '{other}'")),
    }
    Ok(name)
}

fn state_word(state: PrState) -> &'static str {
    state.word()
}

fn assert_badge(_window: &mut Window, cx: &mut App, args: &Args) -> StepResult {
    let branch = args.text(0, "branch")?;
    let expected = args.text(1, "state")?;
    let ci = args.opt(2);
    let ctx = active_ctx(cx)?;
    let full = crate::backend::FullName::try_from(format!("refs/heads/{branch}"))
        .map_err(|e| e.to_string())?;
    let controller = controller(cx)?;
    let controller = controller.read(cx);
    let found = controller.badge(&full);
    let _ = ctx;
    match (found, expected.as_str()) {
        (None, "none") => Ok(format!("{branch} has no badge")),
        (None, _) => Err(format!("{branch} has no badge, expected {expected}")),
        (Some(badge), "none") => Err(format!("{branch} has badge #{}", badge.number)),
        (Some(badge), want) => {
            if state_word(badge.state) != want {
                return Err(format!(
                    "{branch}: expected {want}, found {} (#{})",
                    badge.state.word(),
                    badge.number
                ));
            }
            if let Some(ci) = ci {
                let have =
                    super::badges::ci_state_of(badge).map(|s| format!("{s:?}").to_lowercase());
                let want_ci = match ci.as_str() {
                    "none" => None,
                    "passed" => Some("passed".to_owned()),
                    "failed" => Some("failed".to_owned()),
                    "running" => Some("running".to_owned()),
                    other => return Err(format!("unknown CI state '{other}'")),
                };
                if have != want_ci {
                    return Err(format!("{branch}: CI is {have:?}, expected {want_ci:?}"));
                }
            }
            Ok(format!("{branch}: {want} #{}", badge.number))
        }
    }
}

fn assert_status(_window: &mut Window, cx: &mut App, args: &Args) -> StepResult {
    let want = args.text(0, "status")?;
    let status = controller(cx)?.read(cx).status().clone();
    let have = match status {
        super::ForgeStatus::Starting => "starting",
        super::ForgeStatus::NoForge => "no-forge",
        super::ForgeStatus::NoAccount => "no-account",
        super::ForgeStatus::Idle => "idle",
        super::ForgeStatus::Refreshing => "refreshing",
        super::ForgeStatus::Offline => "offline",
        super::ForgeStatus::RateLimited => "rate-limited",
        super::ForgeStatus::AuthFailed(_) => "auth-failed",
    };
    if have == want {
        Ok(have.to_owned())
    } else {
        Err(format!("the forge status is {have}, expected {want}"))
    }
}

fn busy_probe(_window: &mut Window, cx: &App) -> Option<String> {
    let ctx = active_ctx(cx).ok()?;
    let controller = super::controller_of(ctx.id, cx)?;
    controller
        .read(cx)
        .is_busy()
        .then(|| "the forge controller is working".to_owned())
}

/// Registers the verbs and the idle probe.
pub(super) fn register(cx: &mut App) {
    register_verb(
        cx,
        Verb::new(
            "forge mock start",
            "forge mock start [<branch>=<open|draft>[:<passed|failed|running>]]...",
            "Starts a fake GitHub Enterprise forge on loopback with pull requests for the branches, real services with an \
             in-memory keychain, and points origin at it",
            mock_start,
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "forge mock sign in",
            "forge mock sign in",
            "Connects the fake forge's account (the real sign-in code path)",
            mock_sign_in,
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "forge mock merge",
            "forge mock merge <branch>",
            "Merges the fake pull request of a branch and refreshes",
            mock_merge,
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "forge mock pr",
            "forge mock pr <branch> <open|draft> [passed|failed|running]",
            "Adds a fake pull request and refreshes",
            mock_pr,
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "forge mock offline",
            "forge mock offline on|off",
            "Makes the fake forge drop connections",
            mock_offline,
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "forge refresh",
            "forge refresh",
            "Asks the forge now (like a push does)",
            refresh,
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "open forge dialog",
            "open forge dialog github-token|enterprise|gitlab-token|gitlab-selfhosted|bitbucket|device",
            "Opens a sign-in dialog of the Accounts page",
            open_forge_dialog,
        ),
    );
    register_verb(
        cx,
        Verb::new(
            "assert forge badge",
            "assert forge badge <branch> <open|draft|merged|closed|none> [passed|failed|running|none]",
            "Checks the pull request badge of a local branch",
            assert_badge,
        )
        .without_settle(),
    );
    register_verb(
        cx,
        Verb::new(
            "assert forge status",
            "assert forge status <idle|no-account|offline|no-forge|auth-failed|rate-limited>",
            "Checks how the forge of the tab stands",
            assert_status,
        )
        .without_settle(),
    );
    register_idle_probe(cx, busy_probe);
}

#[cfg(all(test, feature = "qa"))]
mod tests {
    use core::prelude::v1::test;

    use std::time::Duration;

    use gpui_kit::{AppContext as _, TestAppContext};

    use crate::qa::verbs::{Action, lookup};
    use crate::qa::{Args, StepResult, dsl};
    use crate::test_support::{DEFAULT_SIZE, Fixture, mount_fixture};

    fn run_line(
        cx: &mut TestAppContext,
        handle: gpui_kit::AnyWindowHandle,
        text: &str,
    ) -> StepResult {
        let lines = dsl::parse(text).expect("a valid line");
        cx.update_window(handle, |_, window, cx| {
            let (verb, consumed) =
                lookup(cx, &lines[0].tokens).unwrap_or_else(|| panic!("no verb for '{text}'"));
            let args = Args::new(
                lines[0].tokens[consumed..].to_vec(),
                std::path::PathBuf::from("."),
            );
            match &verb.action {
                Action::Sync(handler) => handler(window, cx, &args),
                Action::Runner(kind) => panic!("{kind:?} is a runner verb"),
            }
        })
        .expect("the test window is open")
    }

    fn settle(cx: &mut TestAppContext) {
        for _ in 0..40 {
            cx.run_until_parked();
            cx.executor().advance_clock(Duration::from_millis(100));
        }
        cx.run_until_parked();
    }

    /// The scenario `tools/qa/scenarios/m2-forge.qa` as far as it needs no window capture: the verbs start the fake
    /// forge, sign in, and the badges and the offline state come out as the scenario asserts.
    #[gpui_kit::test]
    fn the_forge_verbs_drive_the_real_paths(cx: &mut TestAppContext) {
        let fixture = Fixture::new();
        fixture.write("README.md", "hello\n");
        fixture.commit_all("Initial commit");
        let bare = fixture.dir.path().join("remote.git");
        let bare_text = bare.to_string_lossy().into_owned();
        fixture.git(&["init", "-q", "--bare", &bare_text]);
        fixture.git(&["remote", "add", "origin", &bare_text]);
        for branch in ["feature/login", "fix/typo", "docs/rewrite"] {
            fixture.git(&["branch", branch]);
        }
        for branch in ["feature/login", "fix/typo"] {
            fixture.git(&[
                "update-ref",
                &format!("refs/remotes/origin/{branch}"),
                "HEAD",
            ]);
            fixture.git(&["config", &format!("branch.{branch}.remote"), "origin"]);
            fixture.git(&[
                "config",
                &format!("branch.{branch}.merge"),
                &format!("refs/heads/{branch}"),
            ]);
        }
        let (handle, _app, _tab) = mount_fixture(cx, &fixture, DEFAULT_SIZE);
        cx.update(|cx| {
            cx.set_global(crate::forge::ActivityOverride(std::rc::Rc::new(
                std::cell::Cell::new(true),
            )))
        });
        let line = |cx: &mut TestAppContext, text: &str| {
            let result = run_line(cx, handle, text);
            settle(cx);
            result
        };
        line(
            cx,
            "forge mock start feature/login=open:passed fix/typo=draft:running",
        )
        .expect("the fake forge starts");
        line(cx, "forge mock sign in").expect("the fake account signs in");
        for text in [
            "assert forge status idle",
            "assert forge badge feature/login open passed",
            "assert forge badge fix/typo draft running",
            "assert forge badge docs/rewrite none",
        ] {
            run_line(cx, handle, text).unwrap_or_else(|e| panic!("{text}: {e}"));
        }
        line(cx, "forge mock merge feature/login").expect("merge");
        run_line(cx, handle, "assert forge badge feature/login merged").expect("merged badge");
        line(cx, "forge mock offline on").expect("offline");
        line(cx, "forge refresh").expect("refresh");
        run_line(cx, handle, "assert forge status offline").expect("offline status");
        run_line(cx, handle, "assert forge badge fix/typo draft running")
            .expect("the cached badge stays while offline");
        assert!(
            run_line(cx, handle, "assert forge badge fix/typo merged").is_err(),
            "a wrong expectation fails"
        );
    }
}
