//! Headless tests of the G10 dialogs: sizes against the metrics, fields, validation, Enter / Escape, and behaviour
//! against the inert backend and fixture repositories.

use core::prelude::v1::test;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::mpsc::sync_channel;
use std::time::Duration;

use gpui_kit::component::WindowExt as _;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, IntoElement as _, SharedString, Styled as _,
    TestAppContext, Window, div,
};

use super::common::DialogHooks;
use crate::backend::present::{DialogAction, ErrorDialogData, HookOutput};
use crate::backend::prompt::ConfirmRequest;
use crate::backend::{
    CancelToken, ConfigScope, CredentialRequest, CredentialResponse, Identity, ObjectId,
    PromptKind, ResetMode, StartPoint,
};
use crate::dialogs::{about, signoff, stash, submodule, tag};
use crate::dialogs::{author, branch, choice, clone, confirm, credentials, error, remote, reset};
use crate::metrics::dialogs as m;
use crate::tab::ops::OpKind;
use crate::test_support::geometry::{assert_height, assert_width, bounds, init_visual, open_probe};
use crate::test_support::seed::{full, oid};
use crate::test_support::{
    DEFAULT_SIZE, Fixture, RepoSeed, TestCall, calls, inert_tab_ctx, mount_fixture, mount_seeded,
    script_op,
};

/// A bare window for dialogs that need no tab.
fn mount(cx: &mut TestAppContext) -> AnyWindowHandle {
    init_visual(cx);
    let (handle, _) = open_probe(cx, 1280., 720., |_, _| div().size_full().into_any_element());
    handle
}

/// Renders twice so a freshly opened dialog layer and its deferred focus settle.
fn settle(cx: &mut TestAppContext, handle: AnyWindowHandle) {
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
    })
    .expect("window");
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .expect("window");
}

fn open_in(
    cx: &mut TestAppContext,
    handle: AnyWindowHandle,
    f: impl FnOnce(&mut Window, &mut App),
) {
    cx.update_window(handle, |_, window, cx| f(window, cx))
        .expect("window");
    settle(cx, handle);
}

fn with_window<R>(
    cx: &mut TestAppContext,
    handle: AnyWindowHandle,
    f: impl FnOnce(&mut Window, &mut App) -> R,
) -> R {
    let result = cx
        .update_window(handle, |_, window, cx| f(window, cx))
        .expect("window");
    cx.run_until_parked();
    result
}

fn click(cx: &mut TestAppContext, handle: AnyWindowHandle, id: &'static str) {
    with_window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click(id, cx);
    });
    settle(cx, handle);
}

fn type_into(cx: &mut TestAppContext, handle: AnyWindowHandle, id: &'static str, text: &str) {
    with_window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click(id, cx);
        window.render_frame(cx);
        window.input(text, cx);
    });
    settle(cx, handle);
}

fn press(cx: &mut TestAppContext, handle: AnyWindowHandle, key: &str) {
    with_window(cx, handle, |window, cx| window.press(key, cx));
    settle(cx, handle);
}

fn present(cx: &mut TestAppContext, handle: AnyWindowHandle, id: &'static str) -> bool {
    with_window(cx, handle, |window, _| window.try_find(id).is_some())
}

fn dialog_open(cx: &mut TestAppContext, handle: AnyWindowHandle) -> bool {
    with_window(cx, handle, |window, cx| window.has_active_dialog(cx))
}

fn left(cx: &mut TestAppContext, handle: AnyWindowHandle, id: &'static str) -> f32 {
    with_window(cx, handle, |window, _| bounds(window, id).origin.x.as_f32())
}

fn size_of(cx: &mut TestAppContext, handle: AnyWindowHandle, id: &'static str) -> (f32, f32) {
    with_window(cx, handle, |window, _| {
        let b = bounds(window, id);
        (b.size.width.as_f32(), b.size.height.as_f32())
    })
}

fn set_hooks(cx: &mut TestAppContext, hooks: DialogHooks) {
    cx.update(|cx| cx.set_global(hooks));
}

// ------------------------------------------------------------------------------------------------------ error

#[gpui_kit::test]
fn error_dialog_is_580_wide_and_ok_is_right_aligned(cx: &mut TestAppContext) {
    let handle = mount(cx);
    let closed = Rc::new(Cell::new(0));
    let on_close = closed.clone();
    open_in(cx, handle, |window, cx| {
        error::open(
            window,
            cx,
            ErrorDialogData::new("Push failed", "The remote rejected the push"),
            Box::new(move |_, _| on_close.set(on_close.get() + 1)),
        );
    });
    assert_eq!(
        size_of(cx, handle, error::ids::ERROR_DIALOG).0,
        580.,
        "the error column is 580 dp wide"
    );
    with_window(cx, handle, |window, _| {
        assert_width(window, crate::ids::DIALOG_FRAME, 612.);
        let ok = bounds(window, error::ids::ERROR_OK);
        let frame = bounds(window, crate::ids::DIALOG_FRAME);
        let right =
            (frame.origin.x + frame.size.width).as_f32() - (ok.origin.x + ok.size.width).as_f32();
        assert!(
            (right - 16.).abs() <= 0.5,
            "OK sits 16 dp from the right edge, got {right}"
        );
    });
    assert!(
        !present(cx, handle, error::ids::ERROR_RETRY)
            && !present(cx, handle, error::ids::ERROR_COPY),
        "a plain error has neither retry nor a details area"
    );
    click(cx, handle, error::ids::ERROR_OK);
    assert!(!dialog_open(cx, handle), "OK closes the dialog");
    assert_eq!(closed.get(), 1, "the queue is told once");
}

#[gpui_kit::test]
fn hook_failed_shows_output_and_extra_action_button(cx: &mut TestAppContext) {
    let handle = mount(cx);
    let extra_ran = Rc::new(Cell::new(false));
    let flag = extra_ran.clone();
    open_in(cx, handle, |window, cx| {
        let mut data =
            ErrorDialogData::new("Commit failed", "The pre-commit hook failed (exit code 1)");
        data.hook = Some(HookOutput {
            hook: "pre-commit".into(),
            exit_code: Some(1),
            output: "lint: 3 errors\nfix them".into(),
        });
        data.extra = Some(DialogAction {
            label: "Open Settings".into(),
            run: Box::new(move |_, _| flag.set(true)),
        });
        error::open(window, cx, data, Box::new(|_, _| {}));
    });
    assert!(
        present(cx, handle, error::ids::ERROR_HOOK_OUTPUT),
        "the hook output is expanded by default"
    );
    let (_, area_h) = size_of(cx, handle, error::ids::ERROR_HOOK_OUTPUT);
    assert!(
        area_h <= 400.5,
        "the output area is at most 400 dp, got {area_h}"
    );
    assert!(
        present(cx, handle, error::ids::ERROR_COPY),
        "the copy button is inside the area"
    );
    assert!(
        present(cx, handle, error::ids::ERROR_EXTRA),
        "the extra action button is rendered"
    );
    with_window(cx, handle, |window, _| {
        let area = bounds(window, error::ids::ERROR_HOOK_OUTPUT);
        let copy = bounds(window, error::ids::ERROR_COPY);
        let right =
            (area.origin.x + area.size.width).as_f32() - (copy.origin.x + copy.size.width).as_f32();
        let bottom = (area.origin.y + area.size.height).as_f32()
            - (copy.origin.y + copy.size.height).as_f32();
        assert!(
            (right - 16.).abs() <= 0.5,
            "the copy button is 16 dp from the right, got {right}"
        );
        assert!(
            (bottom - 16.).abs() <= 0.5,
            "and 16 dp from the bottom, got {bottom}"
        );
        let frame = bounds(window, crate::ids::DIALOG_FRAME);
        assert!(
            frame.origin.y.as_f32() >= 16. - 0.5,
            "the dialog is inside the window"
        );
    });
    let extra_x = left(cx, handle, error::ids::ERROR_EXTRA);
    let ok_x = left(cx, handle, error::ids::ERROR_OK);
    assert!(extra_x < ok_x, "the extra action comes before OK");
    click(cx, handle, error::ids::ERROR_EXTRA);
    assert!(extra_ran.get(), "the extra action ran");
    assert!(!dialog_open(cx, handle), "the dialog closed");
}

#[gpui_kit::test]
fn warning_mode_has_a_single_ok(cx: &mut TestAppContext) {
    let handle = mount(cx);
    open_in(cx, handle, |window, cx| {
        let mut data = ErrorDialogData::new("Hook failed", "The post-checkout hook failed");
        data.warning = true;
        data.hook = Some(HookOutput {
            hook: "post-checkout".into(),
            exit_code: Some(1),
            output: "boom".into(),
        });
        data.extra = Some(DialogAction {
            label: "Never shown".into(),
            run: Box::new(|_, _| {}),
        });
        error::open(window, cx, data, Box::new(|_, _| {}));
    });
    assert!(present(cx, handle, error::ids::ERROR_OK), "OK is there");
    assert!(
        !present(cx, handle, error::ids::ERROR_EXTRA)
            && !present(cx, handle, error::ids::ERROR_RETRY),
        "warning mode has no other button"
    );
    assert!(
        present(cx, handle, error::ids::ERROR_HOOK_OUTPUT),
        "the output is shown"
    );
}

#[gpui_kit::test]
fn error_details_toggle_and_copy_the_real_text(cx: &mut TestAppContext) {
    let handle = mount(cx);
    let copied = Rc::new(RefCell::new(Vec::<String>::new()));
    let sink = copied.clone();
    set_hooks(
        cx,
        DialogHooks {
            copy: Some(Rc::new(move |text| sink.borrow_mut().push(text.to_owned()))),
            ..Default::default()
        },
    );
    open_in(cx, handle, |window, cx| {
        let mut data = ErrorDialogData::new("Fetch failed", "Could not fetch");
        data.details = Some("fatal: unable to access remote".into());
        error::open(window, cx, data, Box::new(|_, _| {}));
    });
    assert!(
        !present(cx, handle, error::ids::ERROR_DETAILS),
        "details are collapsed at first"
    );
    click(cx, handle, error::ids::ERROR_TOGGLE);
    assert!(
        present(cx, handle, error::ids::ERROR_DETAILS),
        "the toggle shows them"
    );
    click(cx, handle, error::ids::ERROR_COPY);
    assert_eq!(
        copied.borrow().as_slice(),
        ["fatal: unable to access remote"],
        "the copy button copies the details, not a made-up trace"
    );
    click(cx, handle, error::ids::ERROR_TOGGLE);
    assert!(
        !present(cx, handle, error::ids::ERROR_DETAILS),
        "and hides them again"
    );
}

#[gpui_kit::test]
fn error_queue_one_at_a_time(cx: &mut TestAppContext) {
    let (handle, app, _tab) = mount_seeded(cx, RepoSeed::linear(1), DEFAULT_SIZE);
    cx.update(|cx| {
        crate::app_view::show_error(cx, ErrorDialogData::new("First", "one"));
        crate::app_view::show_error(cx, ErrorDialogData::new("Second", "two"));
    });
    for _ in 0..3 {
        settle(cx, handle);
    }
    assert_eq!(
        cx.update(|cx| app.read(cx).errors_opened.clone()),
        vec![SharedString::from("First")],
        "only the oldest error is open"
    );
    click(cx, handle, error::ids::ERROR_OK);
    for _ in 0..3 {
        settle(cx, handle);
    }
    assert_eq!(
        cx.update(|cx| app.read(cx).errors_opened.clone()),
        vec![SharedString::from("First"), SharedString::from("Second")],
        "the next one opens after OK"
    );
}

#[gpui_kit::test]
fn hook_failure_of_an_operation_offers_retry_without_hooks(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, RepoSeed::linear(1), DEFAULT_SIZE);
    let retried = Rc::new(Cell::new(false));
    let flag = retried.clone();
    cx.update(|cx| {
        script_op::<crate::backend::OpOutcome<()>>(
            &tab,
            cx,
            Err(crate::backend::present::OpFailure::Git(Arc::new(
                crate::backend::GitError::HookFailed {
                    hook: "pre-commit".into(),
                    exit_code: Some(1),
                    output: "no commit today".into(),
                },
            ))),
            Duration::ZERO,
        )
    });
    with_window(cx, handle, |window, cx| {
        tab.update(cx, |t, cx| {
            let spec = crate::tab::ops::OpSpec::new(OpKind::Commit, |_| {
                Ok(crate::backend::OpOutcome {
                    value: (),
                    refresh: crate::backend::RefreshSet::empty(),
                    log: Default::default(),
                    warnings: Vec::new(),
                })
            })
            .retry_without_hooks(move |_, _, _| flag.set(true));
            t.run_op(spec, window, cx)
        })
    });
    for _ in 0..3 {
        settle(cx, handle);
    }
    assert!(
        present(cx, handle, error::ids::ERROR_HOOK_OUTPUT),
        "the hook output is shown"
    );
    click(cx, handle, error::ids::ERROR_RETRY);
    assert!(retried.get(), "Retry without hooks runs the retry closure");
}

// ---------------------------------------------------------------------------------------------- confirm, choice

fn open_confirm(
    cx: &mut TestAppContext,
    handle: AnyWindowHandle,
    destructive: bool,
) -> Rc<RefCell<Vec<bool>>> {
    let answers = Rc::new(RefCell::new(Vec::new()));
    let sink = answers.clone();
    open_in(cx, handle, |window, cx| {
        let mut request = ConfirmRequest::new("Discard changes?", "This cannot be undone.");
        request.destructive = destructive;
        request.details = vec!["a.txt".into(), "b.txt".into()];
        confirm::open_with(
            window,
            cx,
            request,
            Box::new(move |yes, _, _| sink.borrow_mut().push(yes)),
        );
    });
    answers
}

#[gpui_kit::test]
fn confirm_buttons_answer_yes_and_no(cx: &mut TestAppContext) {
    let handle = mount(cx);
    let answers = open_confirm(cx, handle, true);
    assert_eq!(
        size_of(cx, handle, confirm::ids::CONFIRM_DIALOG).0,
        m::CONFIRM_W
    );
    click(cx, handle, "dialog-cancel");
    assert_eq!(answers.borrow().as_slice(), [false], "Cancel answers no");
    assert!(!dialog_open(cx, handle));
    let answers = open_confirm(cx, handle, true);
    click(cx, handle, "dialog-primary");
    assert_eq!(
        answers.borrow().as_slice(),
        [true],
        "the confirm button answers yes"
    );
}

#[gpui_kit::test]
fn confirm_accepts_enter_unless_destructive(cx: &mut TestAppContext) {
    let handle = mount(cx);
    cx.update(super::register);
    let answers = open_confirm(cx, handle, false);
    press(cx, handle, "enter");
    assert_eq!(
        answers.borrow().as_slice(),
        [true],
        "Enter confirms a harmless question"
    );
    let answers = open_confirm(cx, handle, true);
    press(cx, handle, "enter");
    assert!(
        answers.borrow().is_empty() && dialog_open(cx, handle),
        "Enter never confirms a destructive question"
    );
    press(cx, handle, "escape");
    assert!(!dialog_open(cx, handle), "Escape dismisses it");
    assert!(answers.borrow().is_empty(), "without an answer");
}

#[gpui_kit::test]
fn confirm_reply_channel_gets_the_answer(cx: &mut TestAppContext) {
    let handle = mount(cx);
    let (reply, answer) = sync_channel(1);
    open_in(cx, handle, |window, cx| {
        confirm::open(window, cx, ConfirmRequest::new("Sure?", "Really?"), reply);
    });
    click(cx, handle, "dialog-primary");
    assert_eq!(answer.try_recv(), Ok(true));
}

#[gpui_kit::test]
fn choice_dialog_returns_selection(cx: &mut TestAppContext) {
    let handle = mount(cx);
    let results = Rc::new(RefCell::new(Vec::new()));
    let open = |cx: &mut TestAppContext| {
        let sink = results.clone();
        open_in(cx, handle, |window, cx| {
            choice::open(
                window,
                cx,
                choice::ChoiceSpec {
                    title: "Your branch and its upstream have diverged".into(),
                    message: "Merge or rebase?".into(),
                    options: vec!["Merge".into(), "Rebase".into()],
                    checkbox: Some("Remember for this repository".into()),
                },
                Box::new(move |result, _, _| sink.borrow_mut().push(result)),
            );
        });
    };
    open(cx);
    assert_eq!(
        size_of(cx, handle, choice::ids::CHOICE_DIALOG).0,
        m::CHOICE_W
    );
    click(cx, handle, choice::ids::CHOICE_CHECKBOX);
    with_window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click(("choice-option", 1usize), cx);
    });
    settle(cx, handle);
    open(cx);
    click(cx, handle, choice::ids::CHOICE_CANCEL);
    assert_eq!(
        results.borrow().as_slice(),
        [
            choice::ChoiceResult {
                option: Some(1),
                checked: true
            },
            choice::ChoiceResult {
                option: None,
                checked: false
            },
        ],
        "the chosen option and the checkbox are reported; Cancel is `None`"
    );
}

// ------------------------------------------------------------------------------------------------ credentials

fn request(kind: PromptKind, attempt: u32, cancel: CancelToken) -> CredentialRequest {
    CredentialRequest {
        kind,
        raw_prompt: "prompt: ".into(),
        operation: "Fetch origin".into(),
        repo: None,
        attempt,
        cancel,
    }
}

fn answer_of(response: CredentialResponse) -> Option<String> {
    match response {
        CredentialResponse::Answer(secret) => Some(secret.expose().to_owned()),
        CredentialResponse::Cancel => None,
    }
}

#[test]
fn prompt_texts_cover_every_kind() {
    let cancel = CancelToken::new();
    let kinds = [
        PromptKind::HttpsUsername {
            url: "https://github.com".into(),
        },
        PromptKind::HttpsPassword {
            url: "https://github.com".into(),
            username: Some("bob".into()),
        },
        PromptKind::SshPassphrase {
            key_path: Some("~/.ssh/id_ed25519".into()),
        },
        PromptKind::SshPassword {
            user_host: "git@host".into(),
        },
        PromptKind::SshPin { device: None },
        PromptKind::HostKey {
            host: "example.com".into(),
            key_type: Some("ED25519".into()),
            fingerprint: Some("SHA256:abc".into()),
        },
        PromptKind::Other,
    ];
    let texts: Vec<_> = kinds
        .into_iter()
        .map(|kind| credentials::prompt_texts(&request(kind, 1, cancel.clone())))
        .collect();
    assert_eq!(texts[0].hint, Some("Username"));
    assert!(!texts[0].masked, "a user name is shown");
    assert!(
        texts[1].masked && texts[1].subtitle.contains("bob"),
        "a password is masked"
    );
    assert!(texts[2].subtitle.contains("id_ed25519"));
    assert_eq!(texts[2].title.as_ref(), "Introduce your SSH key's password");
    assert!(texts[3].masked && texts[4].masked);
    assert_eq!(
        (texts[5].primary, texts[5].cancel, texts[5].hint),
        ("Yes", "No", None)
    );
    assert!(texts[5].subtitle.contains("example.com"));
    assert!(
        texts.iter().all(|t| t.subtitle.contains("Fetch origin")),
        "the operation is named"
    );
}

#[test]
fn password_prompt_shows_the_url_without_the_user_name() {
    let cancel = CancelToken::new();
    let texts = |url: &str, username: Option<&str>| {
        credentials::prompt_texts(&request(
            PromptKind::HttpsPassword {
                url: url.into(),
                username: username.map(Into::into),
            },
            1,
            cancel.clone(),
        ))
        .subtitle
    };
    let subtitle = texts("http://qa-user@127.0.0.1:5284/repo.git", Some("qa-user"));
    assert!(
        subtitle.starts_with("Password for qa-user at http://127.0.0.1:5284/repo.git"),
        "the user name is named once: {subtitle}"
    );
    assert!(texts("https://github.com", None).starts_with("Password for https://github.com"));
    assert!(
        texts("https://me@host/a@b", Some("me")).contains("https://host/a@b"),
        "an at sign in the path is kept"
    );
}

#[gpui_kit::test]
fn credential_dialog_kinds(cx: &mut TestAppContext) {
    let handle = mount(cx);
    let cancel = CancelToken::new();
    // User name: a plain field, answered with Enter.
    let (reply, answer) = sync_channel(1);
    open_in(cx, handle, |window, cx| {
        credentials::open(
            window,
            cx,
            request(
                PromptKind::HttpsUsername {
                    url: "https://h".into(),
                },
                1,
                cancel.clone(),
            ),
            reply,
        );
    });
    assert_eq!(
        size_of(cx, handle, crate::ids::DIALOG_FRAME).0,
        332.0,
        "300 dp field + padding"
    );
    assert!(
        !present(cx, handle, credentials::ids::CREDENTIAL_EYE),
        "a user name has no eye toggle"
    );
    type_into(cx, handle, credentials::ids::CREDENTIAL_FIELD, "alice");
    press(cx, handle, "enter");
    assert_eq!(
        answer_of(answer.try_recv().expect("answered")),
        Some("alice".into())
    );
    assert!(!dialog_open(cx, handle), "answering closes the dialog");
    // Password: masked with an eye toggle, answered with the primary button.
    let (reply, answer) = sync_channel(1);
    open_in(cx, handle, |window, cx| {
        credentials::open(
            window,
            cx,
            request(
                PromptKind::HttpsPassword {
                    url: "https://h".into(),
                    username: None,
                },
                1,
                cancel.clone(),
            ),
            reply,
        );
    });
    assert!(
        present(cx, handle, credentials::ids::CREDENTIAL_EYE),
        "a password has the eye toggle"
    );
    with_window(cx, handle, |window, _| {
        let field = bounds(window, credentials::ids::CREDENTIAL_FIELD);
        let eye = bounds(window, credentials::ids::CREDENTIAL_EYE);
        assert_eq!(eye.size.width.as_f32(), 20., "the toggle is 20 dp");
        let end =
            (field.origin.x + field.size.width).as_f32() - (eye.origin.x + eye.size.width).as_f32();
        assert!(
            (end - 4.).abs() <= 0.5,
            "the toggle ends 4 dp before the field's end (09 §0.3), got {end}"
        );
    });
    click(cx, handle, credentials::ids::CREDENTIAL_EYE);
    type_into(cx, handle, credentials::ids::CREDENTIAL_FIELD, "s3cret");
    click(cx, handle, "dialog-primary");
    assert_eq!(
        answer_of(answer.try_recv().expect("answered")),
        Some("s3cret".into())
    );
    // Cancel answers Cancel.
    let (reply, answer) = sync_channel(1);
    open_in(cx, handle, |window, cx| {
        credentials::open(
            window,
            cx,
            request(
                PromptKind::SshPassphrase { key_path: None },
                1,
                cancel.clone(),
            ),
            reply,
        );
    });
    click(cx, handle, "dialog-cancel");
    assert_eq!(answer_of(answer.try_recv().expect("answered")), None);
    // Escape drops the reply: the waiting thread sees a disconnected channel (= Cancel).
    let (reply, answer) = sync_channel(1);
    open_in(cx, handle, |window, cx| {
        credentials::open(
            window,
            cx,
            request(
                PromptKind::SshPassword {
                    user_host: "u@h".into(),
                },
                1,
                cancel.clone(),
            ),
            reply,
        );
    });
    press(cx, handle, "escape");
    assert!(!dialog_open(cx, handle));
    assert!(
        matches!(
            answer.try_recv(),
            Err(std::sync::mpsc::TryRecvError::Disconnected)
        ),
        "Escape drops the reply"
    );
}

#[gpui_kit::test]
fn retry_banner_shows_until_the_first_keystroke(cx: &mut TestAppContext) {
    let handle = mount(cx);
    let (reply, _answer) = sync_channel(1);
    open_in(cx, handle, |window, cx| {
        credentials::open(
            window,
            cx,
            request(
                PromptKind::HttpsPassword {
                    url: "https://h".into(),
                    username: None,
                },
                2,
                CancelToken::new(),
            ),
            reply,
        );
    });
    assert!(
        present(cx, handle, credentials::ids::CREDENTIAL_RETRY),
        "a repeated prompt shows the banner"
    );
    type_into(cx, handle, credentials::ids::CREDENTIAL_FIELD, "x");
    assert!(
        !present(cx, handle, credentials::ids::CREDENTIAL_RETRY),
        "typing hides it"
    );
}

#[gpui_kit::test]
fn credential_dialog_closes_on_cancel_token(cx: &mut TestAppContext) {
    let handle = mount(cx);
    let cancel = CancelToken::new();
    let (reply, answer) = sync_channel(1);
    open_in(cx, handle, |window, cx| {
        credentials::open(
            window,
            cx,
            request(
                PromptKind::SshPassphrase { key_path: None },
                1,
                cancel.clone(),
            ),
            reply,
        );
    });
    assert!(dialog_open(cx, handle));
    cx.executor().advance_clock(Duration::from_millis(250));
    cx.run_until_parked();
    assert!(dialog_open(cx, handle), "a live request keeps its dialog");
    cancel.cancel();
    cx.executor().advance_clock(Duration::from_millis(250));
    cx.run_until_parked();
    settle(cx, handle);
    assert!(
        !dialog_open(cx, handle),
        "the dialog closes within one poll after the token fired"
    );
    assert_eq!(
        answer_of(answer.try_recv().expect("answered")),
        None,
        "and answers Cancel"
    );
}

#[gpui_kit::test]
fn a_cancelled_request_closes_its_own_dialog_not_the_one_above(cx: &mut TestAppContext) {
    let handle = mount(cx);
    let cancel = CancelToken::new();
    let (reply, answer) = sync_channel(1);
    open_in(cx, handle, |window, cx| {
        credentials::open(
            window,
            cx,
            request(
                PromptKind::SshPassphrase { key_path: None },
                1,
                cancel.clone(),
            ),
            reply,
        );
    });
    // Another dialog (here a single-field one) was opened on top of the credential prompt.
    open_in(cx, handle, |window, cx| {
        crate::dialogs::SingleTextFieldDialog::open(
            window,
            cx,
            crate::dialogs::SingleFieldConfig::new(
                crate::icons::AppIcon::Branch,
                "New branch",
                "Create",
                |_, _, _| {},
            ),
        );
    });
    assert!(present(cx, handle, "single-field-input"));
    cancel.cancel();
    cx.executor().advance_clock(Duration::from_millis(250));
    cx.run_until_parked();
    settle(cx, handle);
    assert_eq!(
        answer_of(answer.try_recv().expect("answered")),
        None,
        "the prompt answers Cancel"
    );
    assert!(
        present(cx, handle, "single-field-input"),
        "the dialog above it stays open"
    );
    assert!(
        !present(cx, handle, credentials::ids::CREDENTIAL_DIALOG),
        "the credential prompt itself is gone"
    );
}

#[gpui_kit::test]
fn host_key_dialog_no_yes(cx: &mut TestAppContext) {
    let handle = mount(cx);
    let kind = || PromptKind::HostKey {
        host: "example.com".into(),
        key_type: Some("ED25519".into()),
        fingerprint: Some("SHA256:abcdef".into()),
    };
    for (button, expected) in [("dialog-cancel", "no"), ("dialog-primary", "yes")] {
        let (reply, answer) = sync_channel(1);
        open_in(cx, handle, |window, cx| {
            credentials::open(window, cx, request(kind(), 1, CancelToken::new()), reply);
        });
        assert!(
            present(cx, handle, credentials::ids::CREDENTIAL_FINGERPRINT),
            "the fingerprint is shown"
        );
        assert!(
            !present(cx, handle, credentials::ids::CREDENTIAL_FIELD),
            "there is no text field"
        );
        click(cx, handle, button);
        assert_eq!(
            answer_of(answer.try_recv().expect("answered")),
            Some(expected.into())
        );
    }
}

// ----------------------------------------------------------------------------------------------- clone dialog

#[gpui_kit::test]
fn clone_dialog_is_720_wide_and_validates_in_order(cx: &mut TestAppContext) {
    let handle = mount(cx);
    open_in(cx, handle, clone::open);
    assert_eq!(size_of(cx, handle, clone::ids::CLONE_DIALOG).0, 720.);
    assert_eq!(size_of(cx, handle, crate::ids::DIALOG_FRAME).0, 752.);
    with_window(cx, handle, |window, _| {
        let field = bounds(window, clone::ids::CLONE_DIRECTORY);
        let pick = bounds(window, clone::ids::CLONE_PICK);
        let centre = (field.origin.x + field.size.width).as_f32()
            - (pick.origin.x + pick.size.width / 2.).as_f32();
        assert!(
            (centre - 24.).abs() <= 0.5,
            "the 40 dp search button 4 dp from the end is centred 24 dp from it, got {centre}"
        );
    });
    // Everything empty: the directory is reported first.
    click(cx, handle, clone::ids::CLONE_START);
    assert!(
        present(cx, handle, clone::ids::CLONE_ERROR),
        "the banner appears"
    );
    assert!(dialog_open(cx, handle), "the dialog stays open");
    // Typing clears the error.
    type_into(cx, handle, clone::ids::CLONE_DIRECTORY, "C:/work");
    assert!(
        !present(cx, handle, clone::ids::CLONE_ERROR),
        "editing a field clears the banner"
    );
    click(cx, handle, clone::ids::CLONE_START);
    assert!(
        present(cx, handle, clone::ids::CLONE_ERROR),
        "the empty URL is reported next"
    );
    press(cx, handle, "escape");
    assert!(!dialog_open(cx, handle), "Escape closes the dialog");
}

#[gpui_kit::test]
fn clone_uses_default_directory(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("init");
    let (handle, _app, _tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    let default_dir = fx.dir.path().join("default-clones");
    std::fs::create_dir_all(&default_dir).expect("the default directory can be created");
    cx.update(|cx| {
        let dir = default_dir.clone();
        crate::settings::update(cx, move |s| s.clone_default_directory = Some(dir));
    });
    open_in(cx, handle, clone::open);
    // Only the URL is typed: the directory comes from the settings and the folder from the URL.
    type_into(
        cx,
        handle,
        clone::ids::CLONE_URL,
        &fx.path().to_string_lossy(),
    );
    click(cx, handle, clone::ids::CLONE_START);
    for _ in 0..4 {
        cx.run_until_parked();
        settle(cx, handle);
    }
    assert!(
        default_dir.join("repo").join("a.txt").exists(),
        "the clone landed in the default directory under the folder derived from the URL"
    );
}

#[gpui_kit::test]
fn clone_picker_fills_the_directory_and_clones_a_repository(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("init");
    let (handle, app, _tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    let target_dir = fx.dir.path().join("clones");
    std::fs::create_dir_all(&target_dir).expect("the clone directory can be created");
    let picked = target_dir.clone();
    set_hooks(
        cx,
        DialogHooks {
            pick_folder: Some(Rc::new(move || Some(picked.clone()))),
            ..Default::default()
        },
    );
    open_in(cx, handle, clone::open);
    type_into(
        cx,
        handle,
        clone::ids::CLONE_URL,
        &fx.path().to_string_lossy(),
    );
    click(cx, handle, clone::ids::CLONE_PICK);
    click(cx, handle, clone::ids::CLONE_SAVE_DEFAULT);
    click(cx, handle, clone::ids::CLONE_START);
    for _ in 0..4 {
        cx.run_until_parked();
        settle(cx, handle);
    }
    assert!(
        target_dir.join("repo").join("a.txt").exists(),
        "the folder name `repo` came from the URL and the clone was made in the picked directory"
    );
    assert!(
        !dialog_open(cx, handle),
        "the dialog closes when the clone is open"
    );
    cx.update(|cx| {
        let app = app.read(cx);
        let tab = app.active_tab().expect("the clone opened in a tab");
        assert!(
            tab.read(cx).path().ends_with("repo"),
            "the new tab shows the clone"
        );
        assert_eq!(
            crate::settings::get_or_default(cx).clone_default_directory,
            Some(target_dir.clone()),
            "\"Save as default\" stored the directory after the success"
        );
    });
}

#[gpui_kit::test]
fn clone_shows_progress_and_cancels_when_closed(cx: &mut TestAppContext) {
    // The seeded app's Git runtime never becomes ready, so the clone stays "running".
    let (handle, _app, _tab) = mount_seeded(cx, RepoSeed::linear(1), DEFAULT_SIZE);
    open_in(cx, handle, clone::open);
    type_into(
        cx,
        handle,
        clone::ids::CLONE_URL,
        "https://example.com/team/project.git",
    );
    type_into(cx, handle, clone::ids::CLONE_DIRECTORY, "C:/work");
    assert!(
        !present(cx, handle, clone::ids::CLONE_PROGRESS),
        "the form is showing before Clone is pressed"
    );
    click(cx, handle, clone::ids::CLONE_START);
    assert!(
        present(cx, handle, clone::ids::CLONE_PROGRESS),
        "the cloning state replaced the form"
    );
    assert!(
        !present(cx, handle, clone::ids::CLONE_URL),
        "the form fields are gone"
    );
    click(cx, handle, clone::ids::CLONE_CANCEL);
    assert!(
        !present(cx, handle, clone::ids::CLONE_PROGRESS),
        "Cancel switches to the cancelling state"
    );
    assert!(
        dialog_open(cx, handle),
        "the dialog waits for the clone to stop"
    );
}

#[gpui_kit::test]
fn clone_failure_shows_the_banner_and_keeps_the_dialog(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("init");
    let (handle, _app, _tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    let target_dir = fx.dir.path().join("clones");
    std::fs::create_dir_all(&target_dir).expect("the clone directory can be created");
    set_hooks(
        cx,
        DialogHooks {
            pick_folder: Some(Rc::new({
                let dir = target_dir;
                move || Some(dir.clone())
            })),
            ..Default::default()
        },
    );
    open_in(cx, handle, clone::open);
    let missing = fx.dir.path().join("does-not-exist");
    type_into(
        cx,
        handle,
        clone::ids::CLONE_URL,
        &missing.to_string_lossy(),
    );
    click(cx, handle, clone::ids::CLONE_PICK);
    click(cx, handle, clone::ids::CLONE_START);
    for _ in 0..4 {
        cx.run_until_parked();
        settle(cx, handle);
    }
    assert!(
        dialog_open(cx, handle),
        "a failed clone leaves the dialog open"
    );
    assert!(
        present(cx, handle, clone::ids::CLONE_ERROR),
        "with the error in the banner"
    );
}

// ---------------------------------------------------------------------------- branch, tag, stash single fields

#[test]
fn single_field_configs_have_the_spec_texts() {
    let new_branch = branch::new_branch_config(|_, _, _| {});
    assert_eq!(
        (new_branch.title.as_ref(), new_branch.primary_label.as_ref()),
        ("New branch", "Create branch")
    );
    assert_eq!(
        new_branch.subtitle.as_deref(),
        Some("Create a new branch and check it out")
    );
    assert!(
        (new_branch.is_valid)("x") && !(new_branch.is_valid)("   "),
        "enabled iff not blank"
    );
    let tag = tag::new_tag_config(|_, _, _| {});
    assert_eq!(
        (
            tag.title.as_ref(),
            tag.primary_label.as_ref(),
            tag.subtitle.as_deref()
        ),
        (
            "New tag",
            "Create tag",
            Some("Create a new tag on the specified commit")
        )
    );
    let stash = stash::stash_config(|_, _, _| {});
    assert_eq!(
        (
            stash.title.as_ref(),
            stash.primary_label.as_ref(),
            stash.subtitle.as_deref()
        ),
        (
            "Stash message",
            "Stash",
            Some("Create a new stash with a custom message")
        )
    );
    let rename = branch::rename_branch_config(&full("refs/heads/feature/x"), |_, _, _| {});
    assert_eq!(
        rename.initial_text, "feature/x",
        "the current name is prefilled"
    );
    assert_eq!(
        rename.subtitle.as_deref(),
        Some("Set a new name to the branch \"feature/x\"")
    );
}

#[gpui_kit::test]
fn new_branch_tag_and_stash_dialogs_are_332_wide_and_enter_accepts(cx: &mut TestAppContext) {
    let handle = mount(cx);
    for which in 0..3 {
        let got = Rc::new(RefCell::new(Vec::<String>::new()));
        let sink = got.clone();
        open_in(cx, handle, |window, cx| {
            let on_accept =
                move |text: String, _: &mut Window, _: &mut App| sink.borrow_mut().push(text);
            let config = match which {
                0 => branch::new_branch_config(on_accept),
                1 => tag::new_tag_config(on_accept),
                _ => stash::stash_config(on_accept),
            };
            crate::dialogs::SingleTextFieldDialog::open(window, cx, config);
        });
        assert_eq!(size_of(cx, handle, crate::ids::DIALOG_FRAME).0, 332.0);
        assert!(size_of(cx, handle, "dialog-primary").1 > 0.);
        type_into(cx, handle, "single-field-input", "  v1  ");
        press(cx, handle, "enter");
        assert_eq!(
            got.borrow().as_slice(),
            ["v1"],
            "Enter accepts the trimmed text"
        );
        assert!(!dialog_open(cx, handle));
    }
}

#[gpui_kit::test]
fn rename_dialog_ignores_an_unchanged_name(cx: &mut TestAppContext) {
    let handle = mount(cx);
    let got = Rc::new(RefCell::new(Vec::<String>::new()));
    let sink = got.clone();
    open_in(cx, handle, |window, cx| {
        let config = branch::rename_branch_config(&full("refs/heads/main"), move |name, _, _| {
            sink.borrow_mut().push(name)
        });
        crate::dialogs::register::common::PrefilledFieldDialog::open(window, cx, config);
    });
    assert_eq!(size_of(cx, handle, crate::ids::DIALOG_FRAME).0, 332.0);
    press(cx, handle, "enter");
    assert!(
        got.borrow().is_empty(),
        "accepting the prefilled name does nothing"
    );
    assert!(!dialog_open(cx, handle), "but closes the dialog");
}

#[gpui_kit::test]
fn rename_dialog_selects_the_prefilled_name(cx: &mut TestAppContext) {
    let handle = mount(cx);
    let got = Rc::new(RefCell::new(Vec::<String>::new()));
    let sink = got.clone();
    open_in(cx, handle, |window, cx| {
        let config = branch::rename_branch_config(&full("refs/heads/main"), move |name, _, _| {
            sink.borrow_mut().push(name)
        });
        crate::dialogs::register::common::PrefilledFieldDialog::open(window, cx, config);
    });
    // The text is selected, so typing replaces it.
    with_window(cx, handle, |window, cx| window.input("trunk", cx));
    settle(cx, handle);
    press(cx, handle, "enter");
    assert_eq!(
        got.borrow().as_slice(),
        ["trunk"],
        "typing replaced the selected name"
    );
}

// ----------------------------------------------------------------------------------------------- upstream dialog

fn seed_with_upstream() -> RepoSeed {
    let mut seed = RepoSeed::branchy();
    seed.refs.local[1].upstream = Some(crate::backend::Upstream {
        remote: "origin".into(),
        merge: full("refs/heads/main"),
        tracking: Some(full("refs/remotes/origin/main")),
        gone: false,
    });
    seed.refs.remotes.push(crate::backend::RemoteInfo {
        name: "fork".into(),
        fetch_url: None,
        push_url: None,
        head_branch: None,
        branches: vec![
            crate::backend::RemoteBranch {
                remote: "fork".into(),
                name: "HEAD".into(),
                full: full("refs/remotes/fork/HEAD"),
                target: oid(12),
            },
            crate::backend::RemoteBranch {
                remote: "fork".into(),
                name: "dev".into(),
                full: full("refs/remotes/fork/dev"),
                target: oid(12),
            },
        ],
    });
    seed
}

#[gpui_kit::test]
fn change_upstream_dialog_preselects_and_filters(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, seed_with_upstream(), DEFAULT_SIZE);
    let ctx = cx.update(|cx| tab.read(cx).ctx().clone());
    open_in(cx, handle, |window, cx| {
        branch::open_change_upstream(window, cx, ctx.clone(), full("refs/heads/main"))
    });
    let (w, _) = size_of(cx, handle, branch::ids::UPSTREAM_DIALOG);
    assert_eq!(w, 400., "the content is 400 dp wide");
    let (button_w, _) = size_of(cx, handle, "upstream-remote-button");
    assert_eq!(button_w, 400., "the dropdowns are 400 dp wide");
    assert!(
        size_of(cx, handle, "dialog-primary").0 > 0.,
        "Accept is rendered (enabled: the tracking branch is preselected)"
    );
    // Open the branch dropdown: `HEAD` of the other remote is excluded and the tracking branch is listed.
    click(cx, handle, "upstream-branch-button");
    assert!(
        present(cx, handle, "upstream-branch-option-0"),
        "the tracking branch is listed"
    );
    assert!(
        !present(cx, handle, "upstream-branch-option-1"),
        "only one branch on origin"
    );
    click(cx, handle, "upstream-branch-option-0");
    // Choose the other remote: its branches replace the list and the selection is reset.
    click(cx, handle, "upstream-remote-button");
    click(cx, handle, "upstream-remote-option-0");
    click(cx, handle, "upstream-branch-button");
    assert!(
        present(cx, handle, "upstream-branch-option-0"),
        "fork has `dev`"
    );
    assert!(
        !present(cx, handle, "upstream-branch-option-1"),
        "`HEAD` is not offered as an upstream branch"
    );
}

#[gpui_kit::test]
fn change_upstream_reports_accept_and_cancel_to_the_caller(cx: &mut TestAppContext) {
    for (accept, expected) in [(true, true), (false, false)] {
        let (handle, _app, tab) = mount_seeded(cx, seed_with_upstream(), DEFAULT_SIZE);
        let ctx = cx.update(|cx| tab.read(cx).ctx().clone());
        let answers = Rc::new(RefCell::new(Vec::<bool>::new()));
        let sink = answers.clone();
        open_in(cx, handle, |window, cx| {
            branch::open_change_upstream_then(
                window,
                cx,
                ctx.clone(),
                full("refs/heads/main"),
                Box::new(move |accepted| sink.borrow_mut().push(accepted)),
            )
        });
        assert!(
            answers.borrow().is_empty(),
            "nothing is reported while open"
        );
        if accept {
            click(cx, handle, "dialog-primary");
        } else {
            press(cx, handle, "escape");
        }
        for _ in 0..3 {
            settle(cx, handle);
        }
        assert_eq!(
            answers.borrow().as_slice(),
            [expected],
            "exactly one answer (accept {accept})"
        );
        assert!(!dialog_open(cx, handle));
    }
}

// ------------------------------------------------------------------------------------------------ reset dialog

#[gpui_kit::test]
fn reset_dialog_has_three_380_cards_and_defaults_to_mixed(cx: &mut TestAppContext) {
    let handle = mount(cx);
    let seen = Rc::new(RefCell::new(Vec::<(ObjectId, ResetMode)>::new()));
    let sink = seen.clone();
    open_in(cx, handle, |window, cx| {
        reset::open_with(
            window,
            cx,
            oid(5),
            Rc::new(move |commit, mode, _, _| sink.borrow_mut().push((commit, mode))),
        );
    });
    with_window(cx, handle, |window, _| {
        for ix in 0..3usize {
            let card = bounds(window, ("reset-mode", ix));
            assert_eq!(card.size.width.as_f32(), 380., "card {ix} is 380 dp wide");
        }
        let first = bounds(window, ("reset-mode", 0usize));
        let second = bounds(window, ("reset-mode", 1usize));
        let gap = second.origin.y.as_f32() - (first.origin.y + first.size.height).as_f32();
        assert!(
            (gap - 8.).abs() <= 0.5,
            "the cards are 8 dp apart, got {gap}"
        );
        assert_width(window, crate::ids::DIALOG_FRAME, 412.);
    });
    click(cx, handle, "dialog-primary");
    assert_eq!(
        seen.borrow().as_slice(),
        [(oid(5), ResetMode::Mixed)],
        "Mixed is preselected and the primary button is always enabled"
    );
    let seen2 = Rc::new(RefCell::new(Vec::<(ObjectId, ResetMode)>::new()));
    let sink = seen2.clone();
    open_in(cx, handle, |window, cx| {
        reset::open_with(
            window,
            cx,
            oid(6),
            Rc::new(move |commit, mode, _, _| sink.borrow_mut().push((commit, mode))),
        );
    });
    with_window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click(("reset-mode", 2usize), cx);
    });
    settle(cx, handle);
    click(cx, handle, "dialog-primary");
    assert_eq!(
        seen2.borrow().as_slice(),
        [(oid(6), ResetMode::Hard)],
        "the Hard card was chosen"
    );
}

// ------------------------------------------------------------------------------------------------ remote dialog

#[test]
fn remote_form_drops_a_redundant_push_url() {
    let form = remote::RemoteForm::from_fields(" origin ", " https://a/x ", "https://a/x");
    assert_eq!(form.name, "origin");
    assert_eq!(form.url, "https://a/x");
    assert_eq!(form.push_url, None, "an equal push URL is not stored");
    let form = remote::RemoteForm::from_fields("o", "https://a/x", "ssh://b/x");
    assert_eq!(form.push_url.as_deref(), Some("ssh://b/x"));
    assert_eq!(
        remote::RemoteForm::from_fields("o", "u", "  ").push_url,
        None
    );
}

#[gpui_kit::test]
fn remote_dialog_is_600_wide_and_push_follows_fetch(cx: &mut TestAppContext) {
    let handle = mount(cx);
    let saved = Rc::new(RefCell::new(Vec::<remote::RemoteForm>::new()));
    let sink = saved.clone();
    open_in(cx, handle, |window, cx| {
        remote::open_dialog(
            window,
            cx,
            remote::RemoteTarget::New,
            "",
            "",
            Box::new(move |form, _, _| sink.borrow_mut().push(form)),
        );
    });
    assert_eq!(size_of(cx, handle, remote::ids::REMOTE_DIALOG).0, 600.);
    with_window(cx, handle, |window, _| {
        let frame = bounds(window, crate::ids::DIALOG_FRAME);
        let primary = bounds(window, "dialog-primary");
        let below = (frame.origin.y + frame.size.height).as_f32()
            - (primary.origin.y + primary.size.height).as_f32();
        assert!(
            (below - 8.).abs() <= 0.5,
            "the buttons are 8 dp above the frame's bottom edge (MaterialDialog padding v8), got {below}"
        );
        assert_eq!(
            frame.size.width.as_f32(),
            632.,
            "600 dp column plus padding"
        );
    });
    assert!(
        present(cx, handle, remote::ids::REMOTE_NAME),
        "a new remote has a name field"
    );
    type_into(cx, handle, remote::ids::REMOTE_NAME, "upstream");
    type_into(
        cx,
        handle,
        remote::ids::REMOTE_FETCH,
        "https://example.com/a.git",
    );
    click(cx, handle, "dialog-primary");
    assert_eq!(
        saved.borrow().as_slice(),
        [remote::RemoteForm {
            name: "upstream".into(),
            url: "https://example.com/a.git".into(),
            push_url: None,
        }],
        "the push URL followed the fetch URL (so it is not a separate value)"
    );
    assert!(!dialog_open(cx, handle), "Save changes closes the dialog");
    // Once the push URL differs, the fetch URL no longer drags it along.
    let saved = Rc::new(RefCell::new(Vec::<remote::RemoteForm>::new()));
    let sink = saved.clone();
    open_in(cx, handle, |window, cx| {
        remote::open_dialog(
            window,
            cx,
            remote::RemoteTarget::Edit("origin".into()),
            "https://a",
            "ssh://b",
            Box::new(move |form, _, _| sink.borrow_mut().push(form)),
        );
    });
    assert!(
        !present(cx, handle, remote::ids::REMOTE_NAME),
        "an edit has no name field"
    );
    type_into(cx, handle, remote::ids::REMOTE_FETCH, "/x");
    click(cx, handle, "dialog-primary");
    let form = saved.borrow()[0].clone();
    assert_eq!(form.url, "https://a/x");
    assert_eq!(
        form.push_url.as_deref(),
        Some("ssh://b"),
        "a differing push URL is kept"
    );
}

// --------------------------------------------------------------------------------------------- submodule dialog

#[gpui_kit::test]
fn add_submodule_closes_and_reports_the_form(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("init");
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    let ctx = cx.update(|cx| tab.read(cx).ctx().clone());
    let created = Rc::new(RefCell::new(Vec::<(String, String)>::new()));
    let sink = created.clone();
    open_in(cx, handle, |window, cx| {
        submodule::open_with(
            window,
            cx,
            ctx.clone(),
            Box::new(move |url, dir, _, _| sink.borrow_mut().push((url, dir))),
        );
    });
    assert_eq!(
        size_of(cx, handle, submodule::ids::SUBMODULE_DIALOG).0,
        480.
    );
    // The primary button is disabled while a field is blank: nothing happens.
    click(cx, handle, "dialog-primary");
    assert!(created.borrow().is_empty() && dialog_open(cx, handle));
    type_into(
        cx,
        handle,
        submodule::ids::SUBMODULE_URL,
        "https://example.com/libs/util.git",
    );
    // The directory followed the URL (`util`); Enter in it creates.
    type_into(cx, handle, submodule::ids::SUBMODULE_DIRECTORY, "");
    click(cx, handle, "dialog-primary");
    assert_eq!(
        created.borrow().as_slice(),
        [(
            "https://example.com/libs/util.git".to_owned(),
            "util".to_owned()
        )],
        "the directory defaulted to the repository name"
    );
    assert!(
        !dialog_open(cx, handle),
        "the dialog closes once the form validated"
    );
}

#[gpui_kit::test]
fn submodule_form_errors_show_in_the_banner(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.write("taken/file.txt", "x\n");
    fx.commit_all("init");
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    let ctx = cx.update(|cx| tab.read(cx).ctx().clone());
    let created = Rc::new(Cell::new(0));
    let sink = created.clone();
    open_in(cx, handle, |window, cx| {
        submodule::open_with(
            window,
            cx,
            ctx.clone(),
            Box::new(move |_, _, _, _| sink.set(sink.get() + 1)),
        );
    });
    type_into(
        cx,
        handle,
        submodule::ids::SUBMODULE_URL,
        "https://example.com/libs/taken.git",
    );
    click(cx, handle, "dialog-primary");
    assert!(
        present(cx, handle, submodule::ids::SUBMODULE_ERROR),
        "a directory that contains files is reported in the banner"
    );
    assert_eq!(created.get(), 0, "nothing was created");
    assert!(dialog_open(cx, handle), "the dialog stays open");
    type_into(cx, handle, submodule::ids::SUBMODULE_DIRECTORY, "-fresh");
    assert!(
        !present(cx, handle, submodule::ids::SUBMODULE_ERROR),
        "typing clears the banner"
    );
}

// -------------------------------------------------------------------------------------------------- author

fn seed_with_identity(global: (&str, &str), local: (&str, &str)) -> RepoSeed {
    let mut seed = RepoSeed::linear(1);
    let partial = |(name, email): (&str, &str)| crate::backend::PartialIdentity {
        name: (!name.is_empty()).then(|| name.to_owned()),
        email: (!email.is_empty()).then(|| email.to_owned()),
    };
    seed.identity = crate::backend::IdentityInfo {
        global: partial(global),
        local: partial(local),
        effective: None,
    };
    seed
}

#[test]
fn author_edits_only_changed_scopes() {
    let initial = ("Ann".to_owned(), "ann@x".to_owned());
    let local = ("".to_owned(), "".to_owned());
    assert!(
        author::changed_scopes(&initial, ("Ann", "ann@x"), Some(&local), Some(("", ""))).is_empty()
    );
    let edits = author::changed_scopes(
        &initial,
        ("Ann B", "ann@x"),
        Some(&local),
        Some(("Bob", "bob@y")),
    );
    assert_eq!(edits.len(), 2);
    assert_eq!(edits[0].0, ConfigScope::Global);
    assert_eq!(edits[1].0, ConfigScope::Local);
    let edits = author::changed_scopes(&initial, ("", ""), None, None);
    assert_eq!(
        edits,
        vec![(ConfigScope::Global, None)],
        "clearing both fields unsets the keys"
    );
}

#[gpui_kit::test]
fn author_settings_dialog_saves_changed_values(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(
        cx,
        seed_with_identity(("Ann", "ann@x"), ("", "")),
        DEFAULT_SIZE,
    );
    let ctx = cx.update(|cx| tab.read(cx).ctx().clone());
    open_in(cx, handle, |window, cx| {
        author::open_author_settings(window, cx, Some(ctx.clone()))
    });
    assert_eq!(
        size_of(cx, handle, author::ids::AUTHOR_DIALOG).0,
        416.,
        "400 dp rows with 8 dp padding"
    );
    with_window(cx, handle, |window, _| {
        let name = bounds(window, author::ids::AUTHOR_GLOBAL_NAME);
        let dialog = bounds(window, author::ids::AUTHOR_DIALOG);
        // label column 80 dp (including its 16 dp end padding) after the 8 dp column padding
        assert!(
            ((name.origin.x - dialog.origin.x).as_f32() - 88.).abs() <= 0.5,
            "the field starts after the 80 dp label column, got {name:?}"
        );
    });
    // Nothing changed: Save closes without writing.
    click(cx, handle, "dialog-primary");
    assert!(!dialog_open(cx, handle));
    assert!(
        !cx.update(|cx| calls(&tab, cx)).iter().any(|c| matches!(
            c,
            TestCall::Op {
                kind: OpKind::SaveAuthor,
                ..
            }
        )),
        "unchanged values are not saved"
    );
    // A repository identity makes the override note visible and is saved.
    open_in(cx, handle, |window, cx| {
        author::open_author_settings(window, cx, Some(ctx.clone()))
    });
    type_into(cx, handle, author::ids::AUTHOR_REPO_NAME, "Bob");
    click(cx, handle, "dialog-primary");
    assert!(
        cx.update(|cx| calls(&tab, cx)).iter().any(|c| matches!(
            c,
            TestCall::Op {
                kind: OpKind::SaveAuthor,
                ..
            }
        )),
        "a changed repository identity runs the SaveAuthor operation"
    );
}

#[gpui_kit::test]
fn author_settings_without_a_tab_has_only_global_fields(cx: &mut TestAppContext) {
    let handle = mount(cx);
    open_in(cx, handle, |window, cx| {
        author::open_author_settings(window, cx, None)
    });
    assert!(present(cx, handle, author::ids::AUTHOR_GLOBAL_NAME));
    assert!(
        !present(cx, handle, author::ids::AUTHOR_REPO_NAME),
        "the welcome page edits the global identity only"
    );
    press(cx, handle, "escape");
    assert!(!dialog_open(cx, handle));
}

#[gpui_kit::test]
fn commit_identity_dialog_requires_both_values_and_reports_the_choice(cx: &mut TestAppContext) {
    let (handle, _app, tab) =
        mount_seeded(cx, seed_with_identity(("", ""), ("", "")), DEFAULT_SIZE);
    let ctx = cx.update(|cx| tab.read(cx).ctx().clone());
    let got = Rc::new(RefCell::new(Vec::<author::IdentityChoice>::new()));
    let sink = got.clone();
    open_in(cx, handle, |window, cx| {
        author::open_commit_identity(
            window,
            cx,
            ctx.clone(),
            Rc::new(move |choice, _, _| sink.borrow_mut().push(choice)),
        );
    });
    assert_eq!(size_of(cx, handle, author::ids::IDENTITY_DIALOG).0, 400.);
    click(cx, handle, "dialog-primary");
    assert!(
        got.borrow().is_empty(),
        "Continue does nothing while a value is blank"
    );
    type_into(cx, handle, author::ids::IDENTITY_NAME, "Eve");
    type_into(cx, handle, author::ids::IDENTITY_EMAIL, "eve@example.com");
    click(cx, handle, "dialog-primary");
    assert_eq!(
        got.borrow().as_slice(),
        [author::IdentityChoice::Override(Identity {
            name: "Eve".into(),
            email: "eve@example.com".into()
        })],
        "without the checkbox the identity is used for one commit"
    );
    assert!(!dialog_open(cx, handle));
}

#[gpui_kit::test]
fn commit_identity_saved_choice_continues_after_the_save(cx: &mut TestAppContext) {
    let (handle, _app, tab) =
        mount_seeded(cx, seed_with_identity(("", ""), ("", "")), DEFAULT_SIZE);
    let ctx = cx.update(|cx| tab.read(cx).ctx().clone());
    let got = Rc::new(RefCell::new(Vec::<author::IdentityChoice>::new()));
    let sink = got.clone();
    cx.update(|cx| {
        script_op(
            &tab,
            cx,
            Ok(crate::backend::OpOutcome {
                value: (),
                refresh: crate::backend::RefreshSet::CONFIG,
                log: Default::default(),
                warnings: Vec::new(),
            }),
            Duration::ZERO,
        )
    });
    open_in(cx, handle, |window, cx| {
        author::open_commit_identity(
            window,
            cx,
            ctx.clone(),
            Rc::new(move |choice, _, _| sink.borrow_mut().push(choice)),
        );
    });
    type_into(cx, handle, author::ids::IDENTITY_NAME, "Eve");
    type_into(cx, handle, author::ids::IDENTITY_EMAIL, "eve@example.com");
    click(cx, handle, author::ids::IDENTITY_SAVE);
    click(cx, handle, "dialog-primary");
    for _ in 0..3 {
        settle(cx, handle);
    }
    assert_eq!(
        got.borrow().as_slice(),
        [author::IdentityChoice::Saved(Identity {
            name: "Eve".into(),
            email: "eve@example.com".into()
        })],
        "the continuation ran after the save finished"
    );
    assert!(
        cx.update(|cx| calls(&tab, cx)).iter().any(|c| matches!(
            c,
            TestCall::Op {
                kind: OpKind::SaveAuthor,
                ..
            }
        )),
        "the identity was saved through the SaveAuthor operation"
    );
}

#[gpui_kit::test]
fn save_author_operations_write_the_configuration(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("init");
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    with_window(cx, handle, |window, cx| {
        tab.update(cx, |t, cx| {
            t.save_authors(
                vec![(
                    ConfigScope::Local,
                    Some(Identity {
                        name: "Local Lou".into(),
                        email: "lou@x".into(),
                    }),
                )],
                None,
                window,
                cx,
            )
        })
    });
    for _ in 0..3 {
        settle(cx, handle);
    }
    let name = fx.git(&["config", "--local", "user.name"]);
    assert_eq!(
        name.trim(),
        "Local Lou",
        "the repository identity was written"
    );
}

#[gpui_kit::test]
fn write_commit_graph_creates_the_file(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("init");
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    let started = with_window(cx, handle, |window, cx| {
        tab.update(cx, |t, cx| t.write_commit_graph(window, cx))
    });
    assert!(started, "the operation started");
    for _ in 0..4 {
        settle(cx, handle);
    }
    let graph = fx.path().join(".git/objects/info/commit-graph");
    assert!(graph.exists(), "`git commit-graph write` created {graph:?}");
}

#[gpui_kit::test]
fn write_commit_graph_is_a_recorded_cancellable_operation(cx: &mut TestAppContext) {
    let (handle, _app, tab) = mount_seeded(cx, RepoSeed::linear(2), DEFAULT_SIZE);
    cx.update(|cx| {
        script_op(
            &tab,
            cx,
            Ok(crate::backend::OpOutcome {
                value: (),
                refresh: crate::backend::RefreshSet::LOG,
                log: Default::default(),
                warnings: Vec::new(),
            }),
            Duration::ZERO,
        )
    });
    with_window(cx, handle, |window, cx| {
        tab.update(cx, |t, cx| t.write_commit_graph(window, cx))
    });
    settle(cx, handle);
    assert!(
        cx.update(|cx| calls(&tab, cx)).contains(&TestCall::Op {
            kind: OpKind::WriteCommitGraph,
            title: None
        }),
        "WriteCommitGraph ran through run_op"
    );
}

// ------------------------------------------------------------------------------------------------- sign-off

#[gpui_kit::test]
fn signoff_dialog_saves_the_format_and_flag(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    fx.write("a.txt", "one\n");
    fx.commit_all("init");
    let (handle, _app, tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    let ctx = cx.update(|cx| tab.read(cx).ctx().clone());
    open_in(cx, handle, |window, cx| {
        signoff::open(window, cx, ctx.clone())
    });
    assert_eq!(
        size_of(cx, handle, crate::ids::DIALOG_FRAME).0,
        332.0,
        "300 dp content"
    );
    assert!(
        !present(cx, handle, "dialog-cancel"),
        "the sign-off dialog has no Cancel button"
    );
    click(cx, handle, signoff::ids::SIGNOFF_ENABLED);
    click(cx, handle, "dialog-primary");
    assert!(!dialog_open(cx, handle), "Save closes the dialog");
    let stored = cx.update(|cx| signoff::stored_signoff(cx, fx.path()));
    assert!(stored.enabled, "the flag was stored");
    assert_eq!(
        stored.text,
        signoff::DEFAULT_FORMAT,
        "the default format was kept"
    );
    // Reopening shows the stored values and Escape leaves them alone.
    open_in(cx, handle, |window, cx| {
        signoff::open(window, cx, ctx.clone())
    });
    click(cx, handle, signoff::ids::SIGNOFF_ENABLED);
    press(cx, handle, "escape");
    assert!(
        cx.update(|cx| signoff::stored_signoff(cx, fx.path()))
            .enabled,
        "Escape dismisses without saving"
    );
}

// --------------------------------------------------------------------------------------------------- about

#[gpui_kit::test]
fn about_is_600_by_600_and_lists_helper_status(cx: &mut TestAppContext) {
    let handle = mount(cx);
    let opened = Rc::new(RefCell::new(Vec::<String>::new()));
    let sink = opened.clone();
    set_hooks(
        cx,
        DialogHooks {
            open_url: Some(Rc::new(move |url| sink.borrow_mut().push(url.to_owned()))),
            ..Default::default()
        },
    );
    open_in(cx, handle, about::open);
    with_window(cx, handle, |window, _| {
        assert_width(window, about::ids::ABOUT_DIALOG, 600.);
        assert_height(window, about::ids::ABOUT_DIALOG, 600.);
    });
    assert!(
        present(cx, handle, about::ids::ABOUT_HELPER_ASKPASS),
        "without a Git runtime the helper section says so in one row"
    );
    click(cx, handle, "about-close");
    assert!(!dialog_open(cx, handle), "Close closes the dialog");
    open_in(cx, handle, about::open);
    with_window(cx, handle, |window, cx| {
        window.render_frame(cx);
        window.click(("about-project", 0usize), cx);
    });
    assert_eq!(
        opened.borrow().as_slice(),
        [about::PROJECTS[0].url],
        "a project link opens its address"
    );
}

#[gpui_kit::test]
fn about_shows_helper_status(cx: &mut TestAppContext) {
    let fx = Fixture::new();
    let (_handle, _app, _tab) = mount_fixture(cx, &fx, DEFAULT_SIZE);
    let lines = cx.update(|cx| about::helper_lines(cx));
    assert_eq!(lines.len(), 2, "both helpers are reported");
    assert!(
        lines
            .iter()
            .all(|l| l.text.contains("gitcthulhu") || l.text.contains("missing")),
        "each line names the helper path: {lines:?}"
    );
}

// ------------------------------------------------------------------------------------ widgets added for G10

#[gpui_kit::test]
fn tab_ctx_used_by_dialogs_can_be_inert(cx: &mut TestAppContext) {
    // `with_tab` on a tab that is gone is a no-op, so a dialog accepted after its tab closed does not panic.
    let handle = mount(cx);
    let ctx = cx.update(inert_tab_ctx);
    open_in(cx, handle, |window, cx| {
        branch::open_new_branch(window, cx, ctx.clone(), StartPoint::Head)
    });
    type_into(cx, handle, "single-field-input", "feature");
    press(cx, handle, "enter");
    assert!(
        !dialog_open(cx, handle),
        "accepting with a gone tab closes the dialog quietly"
    );
}
