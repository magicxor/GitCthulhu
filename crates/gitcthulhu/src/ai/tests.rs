use core::prelude::v1::test;

use std::time::Duration;

use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, App, AppContext as _, Entity, TestAppContext, Window};

use super::button::{ButtonAction, ids as button_ids};
use super::controller::AiController;
use super::fake::{Behavior, FakeProvider};
use super::hub::{self, Readiness};
use super::*;
use crate::app;
use crate::backend::ChangeKind;
use crate::backend::ai::{self as backend_ai, AiFailure, AiMissing, AiProvider, AiSettings};
use crate::dialogs::settings::{self as settings_dialog, SettingsPage, ai as page};
use crate::settings;
use crate::status_pane::commit_box::{self, CommitBox};
use crate::tab::RepoTab;
use crate::tab::ops::OpKind;
use crate::test_support::geometry::{assert_height, bounds};
use crate::test_support::{
    DEFAULT_SIZE, Fixture, RepoSeed, mount_fixture, mount_seeded, toast_texts,
};

// ── helpers ──────────────────────────────────────────────────────────────────────────────────────

fn staged_seed() -> RepoSeed {
    RepoSeed::linear(3).with_changes(&[("a.txt", ChangeKind::Added)], &[])
}

/// Starts the real services (blocking) and installs them in the app state.
fn start_services(cx: &mut TestAppContext) {
    let handle = crate::backend::services::start_services(&settings::Settings::default())
        .expect("the services start in a temporary home");
    cx.update(|cx| {
        app::state_mut(cx).expect("the app state exists").services = Some(handle);
    });
}

fn set_ai(cx: &mut TestAppContext, ai: &AiSettings) {
    let value = backend_ai::settings_value(ai);
    cx.update(|cx| settings::update(cx, |s| s.ai = value));
}

fn lm_studio_at(endpoint: &str) -> AiSettings {
    let mut ai = AiSettings {
        provider: Some(AiProvider::LMStudio),
        ..AiSettings::default()
    };
    ai.lmstudio.endpoint = endpoint.to_owned();
    ai.lmstudio.model = "fake-model".into();
    ai
}

/// Applies the settings to the service now and waits for it.
fn reconfigure(cx: &mut TestAppContext) {
    cx.update(|cx| {
        let hub = hub::hub(cx).expect("the hub is installed");
        hub.update(cx, |hub, cx| hub.reconfigure(cx));
    });
    cx.run_until_parked();
}

fn readiness(cx: &mut TestAppContext) -> Readiness {
    cx.update(|cx| hub::readiness(cx))
}

fn reply(pieces: &[&str]) -> Behavior {
    Behavior::Reply {
        pieces: pieces.iter().map(|p| (*p).to_owned()).collect(),
        delay: Duration::ZERO,
    }
}

fn render(cx: &mut TestAppContext, handle: AnyWindowHandle) {
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
    })
    .expect("window");
}

fn controller(cx: &mut TestAppContext, tab: &Entity<RepoTab>) -> Entity<AiController> {
    cx.update(|cx| tab.read(cx).views().ai.clone())
}

fn box_of(cx: &mut TestAppContext, tab: &Entity<RepoTab>) -> Entity<CommitBox> {
    cx.update(|cx| tab.read(cx).views().status.read(cx).commit_box().clone())
}

fn message(cx: &mut TestAppContext, tab: &Entity<RepoTab>) -> String {
    let commit_box = box_of(cx, tab);
    cx.update(|cx| commit_box.read(cx).message(cx))
}

fn type_message(
    cx: &mut TestAppContext,
    tab: &Entity<RepoTab>,
    handle: AnyWindowHandle,
    text: &str,
) {
    let commit_box = box_of(cx, tab);
    cx.update_window(handle, |_, window, cx| {
        commit_box.update(cx, |b, cx| b.set_message(text.to_owned(), window, cx))
    })
    .expect("window");
}

fn click(cx: &mut TestAppContext, handle: AnyWindowHandle, id: impl Into<gpui_kit::ElementId>) {
    let id = id.into();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click(id, cx);
    })
    .expect("window");
    render(cx, handle);
}

fn advance(cx: &mut TestAppContext, ms: u64) {
    cx.executor().advance_clock(Duration::from_millis(ms));
    cx.run_until_parked();
}

fn found(
    cx: &mut TestAppContext,
    handle: AnyWindowHandle,
    id: impl Into<gpui_kit::ElementId>,
) -> bool {
    let id = id.into();
    cx.update_window(handle, |_, window, _| window.try_find(id).is_some())
        .expect("window")
}

/// A fixture repository with one staged change.
fn staged_fixture() -> Fixture {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("init");
    fx.write("a.txt", "two\n");
    fx.git(&["add", "a.txt"]);
    fx
}

fn start_run(
    cx: &mut TestAppContext,
    handle: AnyWindowHandle,
    tab: &Entity<RepoTab>,
    original: &str,
    job: impl FnOnce(
        &crate::backend::CancelToken,
        &mut (dyn FnMut(String) + Send),
    ) -> Result<String, AiFailure>
    + Send
    + 'static,
) {
    let controller = controller(cx, tab);
    let weak = box_of(cx, tab).downgrade();
    let original = original.to_owned();
    cx.update_window(handle, |_, window, cx| {
        controller.update(cx, |c, cx| c.start_with(weak, original, window, cx, job))
    })
    .expect("window");
}

// ── button states and layout (21 §5.3) ───────────────────────────────────────────────────────────

#[test]
fn button_states() {
    let ready = Readiness::Ready {
        provider: "OpenAI".into(),
        model: "gpt".into(),
    };
    let s = |r: &Readiness, staged, op, gen_| button_state(r, staged, op, gen_);

    let generating = s(&ready, 0, true, true);
    assert_eq!(generating.label, "Stop");
    assert!(generating.enabled, "Stop is always clickable");
    assert_eq!(generating.action, ButtonAction::Stop);

    let idle = s(&ready, 2, false, false);
    assert_eq!(idle.label, "Generate");
    assert!(idle.enabled);
    assert_eq!(idle.action, ButtonAction::Generate);
    assert!(
        idle.tooltip.contains("OpenAI") && idle.tooltip.contains("gpt"),
        "{}",
        idle.tooltip
    );

    let nothing_staged = s(&ready, 0, false, false);
    assert!(!nothing_staged.enabled);
    assert_eq!(
        nothing_staged.tooltip,
        "Stage changes to generate a commit message"
    );

    let busy = s(&ready, 2, true, false);
    assert!(!busy.enabled, "an operation is running");
    assert_eq!(busy.action, ButtonAction::None);

    for r in [Readiness::Off, Readiness::NotConfigured(AiMissing::Key)] {
        for staged in [0, 3] {
            let not_configured = s(&r, staged, false, false);
            assert!(not_configured.enabled, "{r:?}: leads to the settings");
            assert_eq!(not_configured.action, ButtonAction::OpenSettings);
            assert_eq!(
                not_configured.tooltip,
                "Set up an AI provider in Settings \u{2192} AI"
            );
        }
    }
    assert!(
        !s(&Readiness::Pending, 2, false, false).enabled,
        "nothing happens before the settings reached the service"
    );
}

#[gpui_kit::test]
fn no_button_without_running_services(cx: &mut TestAppContext) {
    let (handle, _app, _tab) = mount_seeded(cx, staged_seed(), DEFAULT_SIZE);
    render(cx, handle);
    assert!(
        !found(cx, handle, button_ids::BUTTON),
        "the checkbox row keeps Gitnuro's content while AI is not available"
    );
}

#[gpui_kit::test]
fn button_layout_in_the_checkbox_row(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, staged_seed(), DEFAULT_SIZE);
    start_services(cx);
    reconfigure(cx);
    render(cx, handle);
    render(cx, handle);
    cx.update_window(handle, |_, window, _| {
        let row = bounds(window, commit_box::ids::CHECKBOX_ROW);
        let button = bounds(window, button_ids::BUTTON);
        let commit_box = bounds(window, commit_box::COMMIT_BOX);
        assert_height(
            window,
            commit_box::ids::CHECKBOX_ROW,
            crate::metrics::status::CHECKBOX_ROW_H,
        );
        assert_height(
            window,
            commit_box::COMMIT_BOX,
            crate::metrics::status::COMMIT_BOX_H,
        );
        assert_eq!(
            commit_box.size.height.as_f32(),
            192.,
            "the box keeps its height"
        );
        assert_eq!(
            row.size.height.as_f32(),
            28.,
            "the checkbox row stays 28 dp"
        );
        assert!(
            (button.size.height.as_f32() - 24.).abs() <= 0.5,
            "24 dp high: {button:?}"
        );
        // Gitnuro's content of the row is not moved: Amend starts the row, the button keeps clear of it.
        let amend = bounds(window, commit_box::ids::AMEND);
        assert!(
            (amend.origin.x - row.origin.x).as_f32().abs() <= 0.5,
            "Amend stays at the row start: {amend:?} in {row:?}"
        );
        assert!(
            amend.origin.x + amend.size.width <= button.origin.x,
            "the button does not overlap Amend"
        );
        // Padding 8 + icon 16 + gap 4 + label + padding 8.
        assert!(
            button.size.width.as_f32() > 36.,
            "room for the icon and the label: {button:?}"
        );
        assert!(
            ((button.origin.x + button.size.width) - (row.origin.x + row.size.width))
                .as_f32()
                .abs()
                <= 0.5,
            "right-aligned in the row"
        );
        let centre =
            |b: gpui_kit::Bounds<gpui_kit::Pixels>| (b.origin.y + b.size.height / 2.).as_f32();
        assert!(
            (centre(button) - centre(row)).abs() <= 0.5,
            "vertically centred in the row"
        );
    })
    .expect("window");
    let _ = tab;
}

#[gpui_kit::test]
fn ai_button_click_does_not_toggle_amend_with_the_real_element(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, staged_seed(), DEFAULT_SIZE);
    start_services(cx);
    reconfigure(cx);
    render(cx, handle);
    render(cx, handle);
    let amend = |cx: &mut TestAppContext| cx.update(|cx| tab.read(cx).ctx().ui.read(cx).amend);
    click(cx, handle, button_ids::BUTTON);
    assert!(
        !amend(cx),
        "Amend stays off: the checkbox row is content-wide"
    );
    // Not configured: the click opened Settings at the AI page.
    assert!(found(cx, handle, "settings-dialog"), "Settings opened");
    assert!(
        found(cx, handle, "settings-row-ai-provider"),
        "at the AI page"
    );
}

#[gpui_kit::test]
fn not_configured_opens_settings(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, staged_seed(), DEFAULT_SIZE);
    start_services(cx);
    reconfigure(cx);
    assert_eq!(readiness(cx), Readiness::Off);
    let commit_box = box_of(cx, &tab).downgrade();
    let controller = controller(cx, &tab);
    cx.update_window(handle, |_, window, cx| {
        controller.update(cx, |c, cx| c.generate(commit_box, window, cx))
    })
    .expect("window");
    render(cx, handle);
    assert!(found(cx, handle, "settings-dialog"));
    assert!(found(cx, handle, "settings-row-ai-provider"));
    assert!(
        !cx.update(|cx| controller.read(cx).is_generating()),
        "nothing was generated"
    );
}

#[gpui_kit::test]
fn settings_entry_visible_when_available(cx: &mut TestAppContext) {
    let (handle, _app, _tab) = mount_seeded(cx, RepoSeed::linear(1), DEFAULT_SIZE);
    assert!(!cx.update(|cx| settings_available(cx)), "no services yet");
    cx.update(|cx| {
        assert!(
            !settings_dialog::categories(cx)
                .iter()
                .any(|(_, pages)| pages.contains(&SettingsPage::Ai)),
            "milestone-1 list without the services"
        );
    });
    start_services(cx);
    assert!(cx.update(|cx| settings_available(cx)));
    cx.update(|cx| {
        let sections = settings_dialog::categories(cx);
        let integrations = sections
            .iter()
            .find(|(section, _)| *section == settings_dialog::Section::Integrations)
            .expect("an INTEGRATIONS section");
        assert!(integrations.1.contains(&SettingsPage::Ai));
    });
    cx.update_window(handle, |_, window, cx| {
        settings_dialog::open(window, cx, Some(SettingsPage::Ai));
    })
    .expect("window");
    render(cx, handle);
    assert!(found(
        cx,
        handle,
        settings_dialog::ids::entry(SettingsPage::Ai)
    ));
}

// ── generation: streaming, stop, errors ──────────────────────────────────────────────────────────

#[gpui_kit::test]
fn streaming_updates_message_throttled(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, staged_seed(), DEFAULT_SIZE);
    type_message(cx, &tab, handle, "my notes");
    start_run(cx, handle, &tab, "my notes", |_, emit| {
        for text in ["a", "ab", "abc", "abcd"] {
            emit(text.to_owned());
        }
        Ok("abcd final".to_owned())
    });
    let commit_box = box_of(cx, &tab);
    assert!(
        cx.update(|cx| commit_box.read(cx).is_read_only()),
        "the field is read-only while the text streams"
    );
    cx.run_until_parked();
    assert_eq!(
        message(cx, &tab),
        "abcd final",
        "the final text replaces the stream"
    );
    let controller = controller(cx, &tab);
    cx.update(|cx| {
        let c = controller.read(cx);
        assert_eq!(
            c.streamed_applies(),
            1,
            "four chunks that arrive together are applied once, newest only"
        );
        assert!(!c.is_generating());
        assert!(!commit_box.read(cx).is_read_only(), "editable again");
    });
}

#[gpui_kit::test]
fn stop_cancels_and_keeps_text(cx: &mut TestAppContext) {
    let (handle, app, tab) = mount_seeded(cx, staged_seed(), DEFAULT_SIZE);
    type_message(cx, &tab, handle, "notes");
    start_run(cx, handle, &tab, "notes", |cancel, _| {
        // Runs when the executor is parked, after Stop.
        assert!(cancel.is_cancelled(), "Stop cancelled the token");
        Err(AiFailure::Cancelled)
    });
    let controller = controller(cx, &tab);
    let commit_box = box_of(cx, &tab);
    // The text that arrived before Stop (the pump delivers it on the UI thread).
    cx.update_window(handle, |_, window, cx| {
        controller.update(cx, |c, cx| c.apply(1, "partial".to_owned(), window, cx))
    })
    .expect("window");
    assert_eq!(message(cx, &tab), "partial");
    assert!(cx.update(|cx| controller.read(cx).is_generating()));
    cx.update(|cx| {
        controller.update(cx, |c, cx| assert!(c.stop(cx), "a run was stopped"));
        assert!(
            !controller.update(cx, |c, cx| c.stop(cx)),
            "a second Stop finds nothing to stop"
        );
    });
    cx.run_until_parked();
    assert_eq!(message(cx, &tab), "partial", "the text so far stays");
    cx.update(|cx| {
        assert!(!controller.read(cx).is_generating());
        assert!(!commit_box.read(cx).is_read_only());
    });
    assert!(
        cx.update(|cx| toast_texts(&app, cx)).is_empty(),
        "stopping is not an error"
    );
}

#[gpui_kit::test]
fn escape_tab_switch_and_commit_stop_the_run(cx: &mut TestAppContext) {
    let (handle, app, tab) = mount_seeded(cx, staged_seed(), DEFAULT_SIZE);
    let controller = controller(cx, &tab);
    let running = |cx: &mut TestAppContext| cx.update(|cx| controller.read(cx).is_generating());

    start_run(cx, handle, &tab, "", |_, _| Err(AiFailure::Cancelled));
    assert!(running(cx));
    cx.update_window(handle, |_, window, cx| window.press("escape", cx))
        .expect("window");
    assert!(!running(cx), "Escape stops it");

    // With a context menu open, the first Escape closes the menu and the run goes on; the next one stops it.
    start_run(cx, handle, &tab, "", |_, _| Err(AiFailure::Cancelled));
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        crate::menus::open_context_menu(
            window,
            cx,
            gpui_kit::point(gpui_kit::px(200.), gpui_kit::px(200.)),
            vec![crate::menus::MenuItem::action("x-item", "Item", |_, _| {})],
        );
        window.render_frame(cx);
    })
    .expect("window");
    let menu_open = |cx: &mut TestAppContext| {
        cx.update(|cx| {
            cx.global::<crate::menus::MenuHostGlobal>()
                .0
                .read(cx)
                .is_open()
        })
    };
    assert!(menu_open(cx));
    assert!(running(cx), "opening a menu does not stop the generation");
    // (No parking here: the scripted job would finish the run.)
    cx.update_window(handle, |_, window, cx| window.press("escape", cx))
        .expect("window");
    assert!(!menu_open(cx), "Escape closed the menu");
    assert!(running(cx), "and did not stop the generation");
    cx.update_window(handle, |_, window, cx| window.press("escape", cx))
        .expect("window");
    assert!(!running(cx), "the next Escape stops it");

    start_run(cx, handle, &tab, "", |_, _| Err(AiFailure::Cancelled));
    assert!(running(cx));
    cx.update_window(handle, |_, window, cx| {
        app.update(cx, |app, cx| app.new_empty_tab(window, cx));
    })
    .expect("window");
    assert!(!running(cx), "leaving the tab stops it");

    cx.update_window(handle, |_, window, cx| {
        app.update(cx, |app, cx| app.select_tab(0, window, cx));
    })
    .expect("window");
    start_run(cx, handle, &tab, "", |_, _| Err(AiFailure::Cancelled));
    assert!(running(cx));
    let events = cx.update(|cx| tab.read(cx).ctx().events.clone());
    cx.update(|cx| {
        events.update(cx, |_, cx| {
            cx.emit(crate::tab::RepoTabEvent::OpStarted {
                kind: OpKind::StageFile,
            })
        })
    });
    assert!(running(cx), "staging more files does not end the run");
    cx.update(|cx| {
        events.update(cx, |_, cx| {
            cx.emit(crate::tab::RepoTabEvent::OpStarted {
                kind: OpKind::Commit,
            })
        })
    });
    assert!(!running(cx), "committing ends it");
    cx.run_until_parked();
}

#[gpui_kit::test]
fn error_restores_text(cx: &mut TestAppContext) {
    let (handle, app, tab) = mount_seeded(cx, staged_seed(), DEFAULT_SIZE);
    type_message(cx, &tab, handle, "my own words");
    start_run(cx, handle, &tab, "my own words", |_, emit| {
        emit("half a message".to_owned());
        Err(AiFailure::Failed(
            "The AI provider rejected the credentials: nope".to_owned(),
        ))
    });
    cx.run_until_parked();
    assert_eq!(
        message(cx, &tab),
        "my own words",
        "the text the user had comes back, whatever streamed in"
    );
    let commit_box = box_of(cx, &tab);
    assert!(!cx.update(|cx| commit_box.read(cx).is_read_only()));
    let toasts = cx.update(|cx| toast_texts(&app, cx));
    assert!(
        toasts
            .iter()
            .any(|t| t.contains("Couldn't generate the commit message") && t.contains("rejected")),
        "{toasts:?}"
    );
}

#[gpui_kit::test]
fn real_provider_streams_the_message_into_the_box(cx: &mut TestAppContext) {
    let fake = FakeProvider::start(reply(&["feat: add ", "the thing\n\n", "It does stuff."]))
        .expect("the fake provider starts");
    let fx = staged_fixture();
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    start_services(cx);
    set_ai(cx, &lm_studio_at(&fake.endpoint()));
    reconfigure(cx);
    assert_eq!(
        readiness(cx),
        Readiness::Ready {
            provider: "LM Studio".into(),
            model: "fake-model".into()
        }
    );
    type_message(cx, &tab, handle, "refactor of a.txt");
    render(cx, handle);
    render(cx, handle);
    click(cx, handle, button_ids::BUTTON);
    cx.run_until_parked();
    assert_eq!(
        message(cx, &tab),
        "feat: add the thing\n\nIt does stuff.",
        "the answer replaced the notes"
    );
    let requests = fake.requests();
    assert_eq!(requests.len(), 1, "one request");
    let prompt = requests[0].user_prompt();
    assert!(requests[0].streamed(), "the answer is streamed");
    assert!(
        prompt.contains("a.txt") && prompt.contains("+two"),
        "the staged patch: {prompt}"
    );
    assert!(prompt.contains("refactor of a.txt"), "the notes: {prompt}");
    assert!(
        prompt.contains("semantic commit prefix"),
        "the Conventional style"
    );
    let commit_box = box_of(cx, &tab);
    assert!(!cx.update(|cx| commit_box.read(cx).is_read_only()));
}

#[gpui_kit::test]
fn real_provider_failure_restores_the_notes(cx: &mut TestAppContext) {
    let fake = FakeProvider::start(Behavior::Status("401 Unauthorized".into()))
        .expect("the fake provider starts");
    let fx = staged_fixture();
    let (handle, app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    start_services(cx);
    set_ai(cx, &lm_studio_at(&fake.endpoint()));
    reconfigure(cx);
    type_message(cx, &tab, handle, "keep me");
    render(cx, handle);
    render(cx, handle);
    click(cx, handle, button_ids::BUTTON);
    cx.run_until_parked();
    assert_eq!(message(cx, &tab), "keep me");
    let toasts = cx.update(|cx| toast_texts(&app, cx));
    assert!(
        toasts
            .iter()
            .any(|t| t.contains("rejected the credentials")),
        "{toasts:?}"
    );
}

#[gpui_kit::test]
fn nothing_staged_disables_the_button(cx: &mut TestAppContext) {
    let fake = FakeProvider::start(reply(&["x"])).expect("the fake provider starts");
    let (handle, _app, tab) = mount_seeded(cx, RepoSeed::linear(3), DEFAULT_SIZE);
    start_services(cx);
    set_ai(cx, &lm_studio_at(&fake.endpoint()));
    reconfigure(cx);
    render(cx, handle);
    render(cx, handle);
    let controller = controller(cx, &tab);
    click(cx, handle, button_ids::BUTTON);
    assert!(
        fake.requests().is_empty(),
        "a disabled button sends nothing"
    );
    assert!(!cx.update(|cx| controller.read(cx).is_generating()));
}

// ── the privacy confirmation (16 A9) ─────────────────────────────────────────────────────────────

#[test]
fn hosted_providers_are_the_ones_off_this_machine() {
    let mut ai = AiSettings::default();
    assert_eq!(backend_ai::hosted_target(&ai), None, "no provider");
    ai.provider = Some(AiProvider::OpenAi);
    assert_eq!(
        backend_ai::hosted_target(&ai),
        Some(("OpenAI".into(), "api.openai.com".into()))
    );
    ai.openai.custom_endpoint = Some("http://localhost:8000/v1".into());
    assert_eq!(
        backend_ai::hosted_target(&ai),
        None,
        "a local OpenAI-compatible server"
    );
    ai.openai.custom_endpoint = Some("https://llm.corp.example/v1".into());
    assert_eq!(
        backend_ai::hosted_target(&ai),
        Some(("OpenAI".into(), "llm.corp.example".into()))
    );
    ai.provider = Some(AiProvider::Anthropic);
    assert_eq!(
        backend_ai::hosted_target(&ai),
        Some(("Anthropic".into(), "api.anthropic.com".into()))
    );
    ai.provider = Some(AiProvider::OpenRouter);
    assert_eq!(
        backend_ai::hosted_target(&ai),
        Some(("OpenRouter".into(), "openrouter.ai".into()))
    );
    ai.provider = Some(AiProvider::Ollama);
    assert_eq!(
        backend_ai::hosted_target(&ai),
        None,
        "Ollama on this machine"
    );
    ai.ollama.endpoint = "[::1]:11434".into();
    assert_eq!(backend_ai::hosted_target(&ai), None, "IPv6 loopback");
    ai.ollama.endpoint = "gpu-box:11434".into();
    assert_eq!(
        backend_ai::hosted_target(&ai),
        Some(("Ollama".into(), "gpu-box".into())),
        "Ollama on another machine leaves this one"
    );
    ai.provider = Some(AiProvider::LMStudio);
    assert_eq!(
        backend_ai::hosted_target(&ai),
        None,
        "the default LM Studio endpoint"
    );
    ai.lmstudio.endpoint = "http://10.0.0.7:1234/v1".into();
    assert_eq!(
        backend_ai::hosted_target(&ai),
        Some(("LM Studio".into(), "10.0.0.7".into()))
    );
}

#[gpui_kit::test]
fn consent_once_for_hosted(cx: &mut TestAppContext) {
    let (handle, _app, _tab) = mount_seeded(cx, staged_seed(), DEFAULT_SIZE);
    let ran = std::rc::Rc::new(std::cell::Cell::new(0));
    let declined = std::rc::Rc::new(std::cell::Cell::new(0));
    let ask = |cx: &mut TestAppContext| {
        let ran = ran.clone();
        let declined = declined.clone();
        cx.update_window(handle, |_, window, cx| {
            jobs::with_consent(
                window,
                cx,
                move |_, _| ran.set(ran.get() + 1),
                move |_, _| declined.set(declined.get() + 1),
            );
        })
        .expect("window");
        render(cx, handle);
    };

    // A local provider needs no confirmation.
    set_ai(cx, &lm_studio_at("http://127.0.0.1:1234/v1"));
    ask(cx);
    assert_eq!(ran.get(), 1, "runs at once");
    assert!(!found(cx, handle, "dialog-primary"));

    // A hosted one asks; declining runs nothing and asks again next time.
    set_ai(
        cx,
        &AiSettings {
            provider: Some(AiProvider::OpenAi),
            ..AiSettings::default()
        },
    );
    ask(cx);
    assert_eq!(ran.get(), 1, "waits for the answer");
    assert!(
        found(cx, handle, "dialog-primary"),
        "the confirmation is open"
    );
    click(cx, handle, "dialog-cancel");
    assert_eq!(ran.get(), 1, "declined");
    assert_eq!(declined.get(), 1, "the caller learns about the refusal");
    assert!(!cx.update(|cx| settings::get_or_default(cx).ai_hosted_consent));

    // Closing the confirmation without an answer counts as declining.
    ask(cx);
    cx.update_window(handle, |_, window, cx| {
        use gpui_kit::component::WindowExt as _;
        window.close_dialog(cx)
    })
    .expect("window");
    render(cx, handle);
    assert_eq!(ran.get(), 1, "closed: nothing runs");
    assert_eq!(declined.get(), 2, "closed: declined");

    ask(cx);
    click(cx, handle, "dialog-primary");
    assert_eq!(ran.get(), 2, "confirmed: runs");
    assert_eq!(declined.get(), 2);
    assert!(
        cx.update(|cx| settings::get_or_default(cx).ai_hosted_consent),
        "remembered"
    );

    ask(cx);
    assert_eq!(ran.get(), 3, "never asked again");
    assert!(!found(cx, handle, "dialog-primary"));
}

#[gpui_kit::test]
fn generate_with_a_hosted_provider_asks_before_anything_is_sent(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, staged_seed(), DEFAULT_SIZE);
    start_services(cx);
    // A key in the in-memory store makes OpenAI "ready" without any network traffic.
    cx.update(|cx| {
        hub::key_store(cx)
            .store("aiOpenAIKey", "sk-test-not-real")
            .expect("in-memory store");
    });
    set_ai(
        cx,
        &AiSettings {
            provider: Some(AiProvider::OpenAi),
            ..AiSettings::default()
        },
    );
    reconfigure(cx);
    assert!(matches!(readiness(cx), Readiness::Ready { .. }));
    let controller = controller(cx, &tab);
    let commit_box = box_of(cx, &tab).downgrade();
    cx.update_window(handle, |_, window, cx| {
        controller.update(cx, |c, cx| c.generate(commit_box, window, cx))
    })
    .expect("window");
    render(cx, handle);
    assert!(found(cx, handle, "dialog-primary"), "the consent dialog");
    assert!(!cx.update(|cx| controller.read(cx).is_generating()));
    click(cx, handle, "dialog-cancel");
    assert!(
        !cx.update(|cx| controller.read(cx).is_generating()),
        "declined: nothing started"
    );
}

// ── keys and the service configuration ───────────────────────────────────────────────────────────

#[test]
fn key_store_is_write_only_for_the_ui_and_maps_environment_variables() {
    let (store, keys) = backend_ai::KeyStore::in_memory();
    assert_eq!(store.state("aiOpenAIKey"), backend_ai::KeyState::Missing);
    store.store("aiOpenAIKey", "sk-1").expect("stores");
    assert_eq!(store.state("aiOpenAIKey"), backend_ai::KeyState::Stored);
    assert_eq!(
        keys.lock()
            .expect("lock")
            .get("aiOpenAIKey")
            .map(String::as_str),
        Some("sk-1")
    );
    store.delete("aiOpenAIKey").expect("deletes");
    assert_eq!(store.state("aiOpenAIKey"), backend_ai::KeyState::Missing);
    store
        .delete("aiOpenAIKey")
        .expect("deleting nothing is fine");

    assert_eq!(
        backend_ai::environment_variable("aiOpenAIKey"),
        Some("OPENAI_API_KEY")
    );
    assert_eq!(
        backend_ai::environment_variable("aiAnthropicKey"),
        Some("ANTHROPIC_API_KEY")
    );
    assert_eq!(
        backend_ai::environment_variable("aiOpenRouterKey"),
        Some("OPENROUTER_API_KEY")
    );
    assert_eq!(backend_ai::environment_variable("proxy-password"), None);
}

#[gpui_kit::test]
fn the_service_gets_keys_through_the_key_store(cx: &mut TestAppContext) {
    let (_handle, _app, _tab) = mount_seeded(cx, RepoSeed::linear(1), DEFAULT_SIZE);
    start_services(cx);
    set_ai(
        cx,
        &AiSettings {
            provider: Some(AiProvider::Anthropic),
            ..AiSettings::default()
        },
    );
    reconfigure(cx);
    assert_eq!(readiness(cx), Readiness::NotConfigured(AiMissing::Key));
    cx.update(|cx| {
        hub::key_store(cx)
            .store("aiAnthropicKey", "sk-ant-test")
            .expect("stores");
        hub::keys_changed(cx);
    });
    cx.run_until_parked();
    assert_eq!(
        readiness(cx),
        Readiness::Ready {
            provider: "Anthropic".into(),
            model: backend_ai::default_model(AiProvider::Anthropic)
        }
    );
}

// ── the settings page ────────────────────────────────────────────────────────────────────────────

fn open_ai_page(cx: &mut TestAppContext, handle: AnyWindowHandle) {
    cx.update_window(handle, |_, window, cx| {
        settings_dialog::open(window, cx, Some(SettingsPage::Ai));
    })
    .expect("window");
    render(cx, handle);
}

fn page_entity(cx: &mut TestAppContext, handle: AnyWindowHandle) -> Entity<page::AiPage> {
    cx.update_window(handle, |_, window, cx| page::page_entity(window, cx))
        .expect("window")
}

/// Types `text` over the content of `field`.
fn retype(
    cx: &mut TestAppContext,
    handle: AnyWindowHandle,
    field: &Entity<crate::widgets::field::OutlinedField>,
    text: &str,
) {
    cx.update_window(handle, |_, window: &mut Window, cx: &mut App| {
        field.update(cx, |f, cx| f.focus(window, cx));
        window.render_frame(cx);
        window.press("ctrl-a", cx);
        if text.is_empty() {
            window.press("backspace", cx);
        } else {
            window.input(text, cx);
        }
    })
    .expect("window");
    cx.run_until_parked();
}

fn current_ai(cx: &mut TestAppContext) -> AiSettings {
    cx.update(|cx| backend_ai::parse_settings(&settings::get_or_default(cx).ai))
}

/// Opens the provider dropdown and picks `index` (0 = None, then the providers in dropdown order).
fn pick_provider(cx: &mut TestAppContext, handle: AnyWindowHandle, index: usize) {
    click(
        cx,
        handle,
        settings_dialog::ids::select(page::ids::PROVIDER),
    );
    click(
        cx,
        handle,
        settings_dialog::ids::select_option(page::ids::PROVIDER, index),
    );
}

#[gpui_kit::test]
fn settings_page_rows_follow_the_provider(cx: &mut TestAppContext) {
    let (handle, _app, _tab) = mount_seeded(cx, RepoSeed::linear(1), DEFAULT_SIZE);
    start_services(cx);
    open_ai_page(cx, handle);
    let row =
        |cx: &mut TestAppContext, key: &str| found(cx, handle, settings_dialog::ids::row(key));
    assert!(
        row(cx, page::ids::PROVIDER) && row(cx, page::ids::STYLE) && row(cx, page::ids::STATUS)
    );
    assert!(
        !row(cx, page::ids::MODEL) && !row(cx, page::ids::KEY) && !row(cx, page::ids::ENDPOINT),
        "without a provider only provider, style and status show"
    );

    pick_provider(cx, handle, 1); // OpenAI
    assert_eq!(current_ai(cx).provider, Some(AiProvider::OpenAi));
    assert!(row(cx, page::ids::MODEL) && row(cx, page::ids::KEY) && row(cx, page::ids::ENDPOINT));

    pick_provider(cx, handle, 2); // Anthropic: fixed endpoint
    assert_eq!(current_ai(cx).provider, Some(AiProvider::Anthropic));
    assert!(row(cx, page::ids::KEY) && !row(cx, page::ids::ENDPOINT));

    pick_provider(cx, handle, 3); // Ollama: no key
    assert_eq!(current_ai(cx).provider, Some(AiProvider::Ollama));
    assert!(!row(cx, page::ids::KEY) && row(cx, page::ids::ENDPOINT));

    pick_provider(cx, handle, 4); // LM Studio
    pick_provider(cx, handle, 5); // OpenRouter
    assert_eq!(current_ai(cx).provider, Some(AiProvider::OpenRouter));
    assert!(row(cx, page::ids::KEY));
    pick_provider(cx, handle, 0);
    assert_eq!(current_ai(cx).provider, None);

    // The style dropdown writes its own setting.
    click(cx, handle, settings_dialog::ids::select(page::ids::STYLE));
    click(
        cx,
        handle,
        settings_dialog::ids::select_option(page::ids::STYLE, 1),
    );
    assert_eq!(
        cx.update(|cx| settings::get_or_default(cx).ai_commit_style),
        settings::AiCommitStyle::PlainImperative
    );
}

#[gpui_kit::test]
fn settings_validation(cx: &mut TestAppContext) {
    let (handle, _app, _tab) = mount_seeded(cx, RepoSeed::linear(1), DEFAULT_SIZE);
    start_services(cx);
    open_ai_page(cx, handle);
    pick_provider(cx, handle, 4); // LM Studio
    let page = page_entity(cx, handle);
    let (model, endpoint) = cx
        .update(|cx| page.read(cx).provider_fields())
        .expect("the provider fields exist");

    retype(cx, handle, &endpoint, "ftp://example.com");
    assert!(
        cx.update(|cx| page.read(cx).endpoint_error().is_some()),
        "not an HTTP URL"
    );
    assert_eq!(
        current_ai(cx).lmstudio.endpoint,
        AiSettings::default().lmstudio.endpoint,
        "an invalid endpoint is not stored"
    );
    retype(cx, handle, &endpoint, "http://192.168.1.5:1234/v1");
    assert!(cx.update(|cx| page.read(cx).endpoint_error().is_none()));
    assert_eq!(
        current_ai(cx).lmstudio.endpoint,
        "http://192.168.1.5:1234/v1"
    );

    retype(cx, handle, &model, "");
    assert!(
        cx.update(|cx| page.read(cx).model_error().is_some()),
        "the model is required"
    );
    assert_eq!(
        current_ai(cx).lmstudio.model,
        "default",
        "the previous model stays"
    );
    retype(cx, handle, &model, "qwen3-coder");
    assert!(cx.update(|cx| page.read(cx).model_error().is_none()));
    assert_eq!(
        current_ai(cx).lmstudio.model,
        "qwen3-coder",
        "free text survives"
    );

    // Ollama wants host:port.
    pick_provider(cx, handle, 3);
    let (_, endpoint) = cx
        .update(|cx| page.read(cx).provider_fields())
        .expect("fields of Ollama");
    retype(cx, handle, &endpoint, "localhost");
    assert!(
        cx.update(|cx| page.read(cx).endpoint_error().is_some()),
        "no port"
    );
    retype(cx, handle, &endpoint, "localhost:11434");
    assert!(cx.update(|cx| page.read(cx).endpoint_error().is_none()));
    assert_eq!(current_ai(cx).ollama.endpoint, "localhost:11434");
}

#[test]
fn field_validation_texts() {
    assert!(backend_ai::validate_model(AiProvider::OpenAi, "gpt-5").is_ok());
    assert_eq!(
        backend_ai::validate_model(AiProvider::OpenAi, "  "),
        Err("The OpenAI model is required".to_owned())
    );
    assert!(
        backend_ai::validate_endpoint(AiProvider::OpenAi, "").is_ok(),
        "blank = OpenAI's own"
    );
    assert!(backend_ai::validate_endpoint(AiProvider::OpenAi, "not a url").is_err());
    assert!(
        backend_ai::validate_endpoint(AiProvider::OpenRouter, "").is_err(),
        "OpenRouter needs one"
    );
    assert!(backend_ai::validate_endpoint(AiProvider::Ollama, "[::1]:11434").is_ok());
    assert!(backend_ai::validate_endpoint(AiProvider::Ollama, "::1:11434").is_err());
    assert!(
        backend_ai::validate_endpoint(AiProvider::LMStudio, "http://localhost:1234/v1").is_ok()
    );
}

#[gpui_kit::test]
fn api_key_is_stored_in_the_key_store_never_in_settings(cx: &mut TestAppContext) {
    let (handle, _app, _tab) = mount_seeded(cx, RepoSeed::linear(1), DEFAULT_SIZE);
    start_services(cx);
    open_ai_page(cx, handle);
    pick_provider(cx, handle, 1); // OpenAI
    cx.run_until_parked();
    let page = page_entity(cx, handle);
    assert_eq!(
        cx.update(|cx| page.read(cx).key_state().cloned()),
        Some(backend_ai::KeyState::Missing)
    );
    assert!(!found(cx, handle, page::ids::KEY_STORED));
    reconfigure(cx);
    assert_eq!(readiness(cx), Readiness::NotConfigured(AiMissing::Key));

    let key = cx.update(|cx| page.read(cx).key_field());
    retype(cx, handle, &key, "sk-secret-123");
    advance(cx, 100);
    assert_eq!(
        cx.update(|cx| hub::key_store(cx).state("aiOpenAIKey")),
        backend_ai::KeyState::Missing,
        "nothing is stored while typing"
    );
    advance(cx, metrics::KEY_DEBOUNCE_MS);
    render(cx, handle);
    assert_eq!(
        cx.update(|cx| hub::key_store(cx).state("aiOpenAIKey")),
        backend_ai::KeyState::Stored,
        "stored after the quiet period"
    );
    assert!(
        found(cx, handle, page::ids::KEY_STORED),
        "the Stored badge shows"
    );
    assert_eq!(
        cx.update(|cx| page.read(cx).key_field().read(cx).text(cx).to_string()),
        "",
        "the field is write-only: the key is gone from it"
    );
    let json =
        cx.update(|cx| serde_json::to_string(&settings::get_or_default(cx)).expect("serialises"));
    assert!(
        !json.contains("sk-secret-123"),
        "the settings file never holds the key"
    );
    advance(cx, 300);
    assert!(
        matches!(readiness(cx), Readiness::Ready { .. }),
        "the service picked the key up: {:?}",
        readiness(cx)
    );

    click(cx, handle, page::ids::KEY_REMOVE);
    assert_eq!(
        cx.update(|cx| hub::key_store(cx).state("aiOpenAIKey")),
        backend_ai::KeyState::Missing,
        "Remove deletes it"
    );
    assert!(!found(cx, handle, page::ids::KEY_STORED));
    advance(cx, 300);
    assert_eq!(readiness(cx), Readiness::NotConfigured(AiMissing::Key));
}

#[gpui_kit::test]
fn a_key_typed_for_one_provider_is_never_stored_for_another(cx: &mut TestAppContext) {
    let (handle, _app, _tab) = mount_seeded(cx, RepoSeed::linear(1), DEFAULT_SIZE);
    start_services(cx);
    open_ai_page(cx, handle);
    pick_provider(cx, handle, 1); // OpenAI
    cx.run_until_parked();
    let page = page_entity(cx, handle);
    let key = cx.update(|cx| page.read(cx).key_field());
    retype(cx, handle, &key, "sk-openai-key");
    advance(cx, 100);
    pick_provider(cx, handle, 2); // Anthropic, before the quiet period ended
    advance(cx, metrics::KEY_DEBOUNCE_MS * 2);
    for handle_name in ["aiOpenAIKey", "aiAnthropicKey"] {
        assert_eq!(
            cx.update(|cx| hub::key_store(cx).state(handle_name)),
            backend_ai::KeyState::Missing,
            "{handle_name}: nothing stored"
        );
    }
    assert_eq!(
        cx.update(|cx| page.read(cx).key_field().read(cx).text(cx).to_string()),
        "",
        "the field was emptied with the provider change"
    );
}

#[gpui_kit::test]
fn test_connection_reports_the_result_and_the_status_row(cx: &mut TestAppContext) {
    let fake = FakeProvider::start(reply(&["pong"])).expect("the fake provider starts");
    let (handle, _app, _tab) = mount_seeded(cx, RepoSeed::linear(1), DEFAULT_SIZE);
    start_services(cx);
    set_ai(cx, &lm_studio_at(&fake.endpoint()));
    reconfigure(cx);
    open_ai_page(cx, handle);
    let page = page_entity(cx, handle);

    click(cx, handle, page::ids::TEST);
    cx.run_until_parked();
    assert_eq!(
        cx.update(|cx| page.read(cx).test_state().clone()),
        page::TestState::Ok
    );
    let requests = fake.requests();
    assert_eq!(requests.len(), 1);
    assert!(!requests[0].streamed(), "one tiny non-streaming request");

    fake.set_behavior(Behavior::Status("401 Unauthorized".into()));
    click(cx, handle, page::ids::TEST);
    cx.run_until_parked();
    match cx.update(|cx| page.read(cx).test_state().clone()) {
        page::TestState::Failed(reason) => {
            assert!(reason.contains("rejected the credentials"), "{reason}")
        }
        other => panic!("expected a failure, got {other:?}"),
    }
}

#[gpui_kit::test]
fn a_test_result_for_settings_changed_meanwhile_is_dropped(cx: &mut TestAppContext) {
    let fake = FakeProvider::start(reply(&["pong"])).expect("the fake provider starts");
    let (handle, _app, _tab) = mount_seeded(cx, RepoSeed::linear(1), DEFAULT_SIZE);
    start_services(cx);
    set_ai(cx, &lm_studio_at(&fake.endpoint()));
    reconfigure(cx);
    open_ai_page(cx, handle);
    let page = page_entity(cx, handle);
    // Start the test, then change the model before its answer is applied.
    cx.update(|cx| page.update(cx, |page, cx| page.test_connection(cx)));
    assert_eq!(
        cx.update(|cx| page.read(cx).test_state().clone()),
        page::TestState::Running
    );
    let (model, _) = cx.update(|cx| page.read(cx).provider_fields().expect("the fields"));
    retype(cx, handle, &model, "another-model");
    cx.run_until_parked();
    assert_eq!(
        cx.update(|cx| page.read(cx).test_state().clone()),
        page::TestState::Idle,
        "the answer was about the old model"
    );
}

#[gpui_kit::test]
fn status_row_names_what_is_missing(cx: &mut TestAppContext) {
    let (handle, _app, _tab) = mount_seeded(cx, RepoSeed::linear(1), DEFAULT_SIZE);
    start_services(cx);
    set_ai(
        cx,
        &AiSettings {
            provider: Some(AiProvider::OpenRouter),
            ..AiSettings::default()
        },
    );
    reconfigure(cx);
    open_ai_page(cx, handle);
    assert_eq!(readiness(cx), Readiness::NotConfigured(AiMissing::Key));
    assert!(found(
        cx,
        handle,
        settings_dialog::ids::row(page::ids::STATUS)
    ));
}

// ── the hooks for other packages ─────────────────────────────────────────────────────────────────

#[gpui_kit::test]
fn pr_text_and_branch_name_hooks_run_against_the_provider(cx: &mut TestAppContext) {
    let fake = FakeProvider::start(reply(&["Add the thing\n\n", "- one change"]))
        .expect("the fake provider starts");
    let (handle, _app, _tab) = mount_seeded(cx, RepoSeed::linear(1), DEFAULT_SIZE);
    start_services(cx);
    set_ai(cx, &lm_studio_at(&fake.endpoint()));
    reconfigure(cx);
    assert!(cx.update(|cx| is_ready(cx)));

    let done = std::rc::Rc::new(std::cell::RefCell::new(None));
    let sink = done.clone();
    let pr_handle = cx
        .update_window(handle, |_, window, cx| {
            generate_pr_text(
                window,
                cx,
                backend_ai::PrRequest {
                    title: String::new(),
                    body: String::new(),
                    commit_messages: vec!["Add the thing".into()],
                },
                |_, _, _| {},
                move |result, _, _| *sink.borrow_mut() = Some(result),
            )
        })
        .expect("window");
    cx.run_until_parked();
    drop(pr_handle);
    let text = done
        .borrow_mut()
        .take()
        .expect("finished")
        .expect("generated");
    assert_eq!(text.title, "Add the thing");
    assert!(text.body.contains("one change"), "{text:?}");

    fake.set_behavior(reply(&["Feat/Add Search"]));
    let name = std::rc::Rc::new(std::cell::RefCell::new(None));
    let sink = name.clone();
    let name_handle = cx
        .update_window(handle, |_, window, cx| {
            suggest_branch_name(
                window,
                cx,
                BranchNameSource::CommitMessage("Add search".into()),
                move |result, _, _| *sink.borrow_mut() = Some(result),
            )
        })
        .expect("window");
    cx.run_until_parked();
    drop(name_handle);
    assert_eq!(
        name.borrow_mut().take().expect("finished").expect("a name"),
        "feat/add-search"
    );
}

#[gpui_kit::test]
fn hooks_report_an_unset_provider_at_once(cx: &mut TestAppContext) {
    let (handle, _app, _tab) = mount_seeded(cx, RepoSeed::linear(1), DEFAULT_SIZE);
    let result = std::rc::Rc::new(std::cell::RefCell::new(None));
    let sink = result.clone();
    // The typical caller: a dialog entity that starts the request from its own update and whose answer updates it
    // again. Answering inside the call would panic with "already being updated".
    let caller = cx.update(|cx| cx.new(|_| 0usize));
    let answered = caller.clone();
    cx.update_window(handle, |_, window, cx| {
        caller.update(cx, |_, cx| {
            suggest_branch_name(
                window,
                cx,
                BranchNameSource::CommitMessage("x".into()),
                move |r, _, cx| {
                    answered.update(cx, |n, _| *n += 1);
                    *sink.borrow_mut() = Some(r);
                },
            );
        });
        assert!(
            result.borrow().is_none(),
            "never answered inside the caller's call"
        );
    })
    .expect("window");
    cx.run_until_parked();
    assert_eq!(cx.update(|cx| *caller.read(cx)), 1);
    assert!(
        matches!(result.borrow().as_ref(), Some(Err(AiFailure::Failed(_)))),
        "answered on the next turn"
    );
}

#[gpui_kit::test]
fn dropping_a_job_cancels_it(cx: &mut TestAppContext) {
    let (handle, _app, _tab) = mount_seeded(cx, RepoSeed::linear(1), DEFAULT_SIZE);
    let job = cx
        .update_window(handle, |_, window, cx| {
            jobs::drive(
                window,
                cx,
                |_cancel, _emit: &mut (dyn FnMut(()) + Send)| Ok::<(), AiFailure>(()),
                |_, _, _| {},
                |_, _, _| {},
            )
        })
        .expect("window");
    let token = job.token();
    assert!(!token.is_cancelled());
    drop(job);
    assert!(
        token.is_cancelled(),
        "dropping the handle cancels the request"
    );
    cx.run_until_parked();
}

#[gpui_kit::test]
fn no_streamed_text_arrives_after_the_result(cx: &mut TestAppContext) {
    let (handle, _app, _tab) = mount_seeded(cx, RepoSeed::linear(1), DEFAULT_SIZE);
    let events = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
    let (items, done) = (events.clone(), events.clone());
    let _job = cx
        .update_window(handle, |_, window, cx| {
            jobs::drive(
                window,
                cx,
                |_cancel, emit: &mut (dyn FnMut(String) + Send)| {
                    for piece in ["a", "ab", "abc"] {
                        emit(piece.to_owned());
                    }
                    Ok::<_, AiFailure>("abc (final)".to_owned())
                },
                move |text, _, _| items.borrow_mut().push(text),
                move |result, _, _| done.borrow_mut().push(format!("done: {}", result.unwrap())),
            )
        })
        .expect("window");
    for _ in 0..10 {
        advance(cx, 50);
    }
    let events = events.borrow();
    assert_eq!(
        events.last().map(String::as_str),
        Some("done: abc (final)"),
        "the result comes last: {events:?}"
    );
    assert_eq!(events.iter().filter(|e| e.starts_with("done")).count(), 1);
}

#[test]
fn failures_read_as_one_sentence() {
    use backend_ai::AiError;
    assert!(
        AiFailure::from(AiError::Auth("bad key".into()))
            .message()
            .contains("bad key")
    );
    assert!(
        AiFailure::from(AiError::RateLimited)
            .message()
            .contains("limiting")
    );
    assert_eq!(AiFailure::from(AiError::Cancelled), AiFailure::Cancelled);
    assert!(matches!(
        AiFailure::from(AiError::NotConfigured(AiMissing::Key)),
        AiFailure::NotConfigured(_)
    ));
    assert_eq!(
        AiFailure::from(AiError::EmptyAnswer).message(),
        "The AI provider answered with nothing"
    );
}
