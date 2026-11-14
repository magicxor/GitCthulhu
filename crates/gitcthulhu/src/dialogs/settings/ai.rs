//! The AI settings page (D0.2; owned by X3 after G0b; spec 21 §5.4; 16 A10): provider, model, API key (write-only,
//! kept in the OS keychain), endpoint, commit message style and a status row with "Test connection". Changes apply
//! immediately like everywhere in Settings; only valid values are stored.

use gpui_kit::{AnyElement, App, Window};
#[cfg(not(feature = "services"))]
use gpui_kit::{IntoElement as _, div};

/// The page content. Without the `services` feature (no AI) the page is empty and never listed.
pub fn page(window: &mut Window, cx: &mut App) -> AnyElement {
    #[cfg(feature = "services")]
    {
        imp::page(window, cx)
    }
    #[cfg(not(feature = "services"))]
    {
        let _ = (window, cx);
        div().into_any_element()
    }
}

/// The page entity, its states and its element ids (tests, QA).
#[cfg(feature = "services")]
pub use imp::{AiPage, TestState, ids, page_entity};

#[cfg(feature = "services")]
mod imp {
    use std::cell::Cell;
    use std::rc::Rc;

    use gpui_kit::base::POPUP_PRIORITY;
    use gpui_kit::prelude::FluentBuilder as _;
    use gpui_kit::{
        AnyElement, App, AppContext as _, BoxShadow, Context, Entity, Global,
        InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, ParentElement as _,
        Pixels, Render, SharedString, Size, StatefulInteractiveElement as _, Styled as _,
        Subscription, Task, TestSupportExt as _, Window, anchored, deferred, div, point, px,
    };
    use std::collections::HashMap;

    use super::super::{SettingRow, ids as settings_ids};
    use crate::ai::{self, KEY_DEBOUNCE_MS, Readiness};
    use crate::backend::CancelToken;
    use crate::backend::ai::{self as backend_ai, AiProvider, AiSettings, KeyState};
    use crate::icons::{AppIcon, icon};
    use crate::metrics::{self, dp, settings as sm};
    use crate::notifications::ToastKind;
    use crate::settings::{self, AiCommitStyle};
    use crate::theme::{ActiveTokens as _, TextStyleExt as _};
    use crate::widgets::field::{FieldEvent, OutlinedField};
    use crate::widgets::{BoundsCell, SecondaryButton, observed, track_bounds, with_layer};

    /// Element ids of the page.
    pub mod ids {
        /// The provider dropdown (row key; the trigger is `settings-select-ai-provider`).
        pub const PROVIDER: &str = "ai-provider";
        /// The commit style dropdown.
        pub const STYLE: &str = "ai-style";
        /// The model field.
        pub const MODEL: &str = "ai-model";
        /// The API key field.
        pub const KEY: &str = "ai-key";
        /// The endpoint field.
        pub const ENDPOINT: &str = "ai-endpoint";
        /// The "Stored" badge next to the key field.
        pub const KEY_STORED: &str = "settings-ai-key-stored";
        /// The button that removes the stored key.
        pub const KEY_REMOVE: &str = "settings-ai-key-remove";
        /// The "Test connection" button.
        pub const TEST: &str = "settings-ai-test";
        /// The status row.
        pub const STATUS: &str = "ai-status";
    }

    /// Applies a dropdown choice to the page.
    type Pick = Rc<dyn Fn(&mut AiPage, &mut Window, &mut Context<AiPage>)>;

    /// The outcome of the last "Test connection".
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum TestState {
        /// Not run (or the settings changed since).
        Idle,
        /// Running.
        Running,
        /// The provider answered.
        Ok,
        /// It failed (reason).
        Failed(String),
    }

    /// The model and endpoint fields of the selected provider (recreated when the provider changes, because the
    /// hints depend on it) with their subscriptions.
    struct ProviderFields {
        model: Entity<OutlinedField>,
        endpoint: Entity<OutlinedField>,
        _subscriptions: Vec<Subscription>,
    }

    /// The AI page entity (one per app; it outlives the dialog).
    pub struct AiPage {
        fields: Option<ProviderFields>,
        key: Entity<OutlinedField>,
        shown_provider: Option<AiProvider>,
        /// The settings the fields were last brought in line with.
        last_seen: AiSettings,
        key_state: Option<KeyState>,
        key_task: Option<Task<()>>,
        test: TestState,
        test_cancel: Option<CancelToken>,
        /// Identity of the newest connection test: a result of an older one (the settings changed since) is dropped.
        test_run: u64,
        model_error: Option<String>,
        endpoint_error: Option<String>,
        open_select: Option<&'static str>,
        select_bounds: HashMap<&'static str, BoundsCell>,
        viewport: Size<Pixels>,
        _subscriptions: Vec<Subscription>,
    }

    /// The global handle of the page entity.
    struct PageGlobal(Entity<AiPage>);

    impl Global for PageGlobal {}

    /// The page entity, created on first use.
    pub fn page_entity(window: &mut Window, cx: &mut App) -> Entity<AiPage> {
        if let Some(page) = cx.try_global::<PageGlobal>().map(|g| g.0.clone()) {
            return page;
        }
        let page = cx.new(|cx| AiPage::new(window, cx));
        cx.set_global(PageGlobal(page.clone()));
        page
    }

    /// The page content.
    pub fn page(window: &mut Window, cx: &mut App) -> AnyElement {
        ai::hub::ensure(cx);
        let page = page_entity(window, cx);
        page.update(cx, |page, cx| page.sync(window, cx));
        page.into_any_element()
    }

    fn current_ai(cx: &App) -> AiSettings {
        backend_ai::parse_settings(&settings::get_or_default(cx).ai)
    }

    impl AiPage {
        fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
            let key = cx.new(|cx| {
                OutlinedField::password(settings_ids::field(ids::KEY), window, cx).hint(
                    "Paste your API key",
                    window,
                    cx,
                )
            });
            let mut subscriptions =
                vec![
                    cx.subscribe_in(&key, window, |this, _, event: &FieldEvent, window, cx| {
                        match event {
                            FieldEvent::Changed => this.key_changed(window, cx),
                            FieldEvent::Enter => this.store_key(window, cx),
                        }
                    }),
                ];
            if let Some(hub) = ai::hub::hub(cx) {
                subscriptions.push(cx.observe(&hub, |_, _, cx| cx.notify()));
            }
            let mut page = Self {
                fields: None,
                key,
                shown_provider: None,
                last_seen: AiSettings::default(),
                key_state: None,
                key_task: None,
                test: TestState::Idle,
                test_cancel: None,
                test_run: 0,
                model_error: None,
                endpoint_error: None,
                open_select: None,
                select_bounds: HashMap::new(),
                viewport: window.viewport_size(),
                _subscriptions: subscriptions,
            };
            page.sync(window, cx);
            page.probe_key(cx);
            page
        }

        /// The outcome of the last connection test.
        pub fn test_state(&self) -> &TestState {
            &self.test
        }

        /// What the key store reported for the selected provider's key (`None` until it answered).
        pub fn key_state(&self) -> Option<&KeyState> {
            self.key_state.as_ref()
        }

        /// The error shown under the model row.
        pub fn model_error(&self) -> Option<&str> {
            self.model_error.as_deref()
        }

        /// The error shown under the endpoint row.
        pub fn endpoint_error(&self) -> Option<&str> {
            self.endpoint_error.as_deref()
        }

        /// The model and endpoint fields (while a provider is selected).
        pub fn provider_fields(&self) -> Option<(Entity<OutlinedField>, Entity<OutlinedField>)> {
            self.fields
                .as_ref()
                .map(|f| (f.model.clone(), f.endpoint.clone()))
        }

        /// The API key field.
        pub fn key_field(&self) -> Entity<OutlinedField> {
            self.key.clone()
        }

        /// Brings the fields in line with the settings: a new provider rebuilds them, an outside change of a
        /// stored value replaces the text. What the user is typing is never overwritten with the same value.
        pub fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
            let ai = current_ai(cx);
            if self.shown_provider != ai.provider
                || (self.fields.is_none() && ai.provider.is_some())
            {
                self.shown_provider = ai.provider;
                self.model_error = None;
                self.endpoint_error = None;
                self.reset_test();
                self.key_state = None;
                // A key typed for the previous provider must never be stored under the new provider's handle.
                self.key_task = None;
                if !self.key.read(cx).text(cx).is_empty() {
                    self.key.update(cx, |f, cx| f.set_text("", window, cx));
                }
                self.build_fields(ai.provider, &ai, window, cx);
                self.probe_key(cx);
                cx.notify();
            } else if ai != self.last_seen
                && let (Some(provider), Some(fields)) = (ai.provider, &self.fields)
            {
                let wanted = [
                    (
                        &fields.model,
                        backend_ai::model_of(&ai, provider).to_owned(),
                    ),
                    (&fields.endpoint, backend_ai::endpoint_of(&ai, provider)),
                ];
                for (field, text) in wanted {
                    if field.read(cx).text(cx).as_ref() != text {
                        field.update(cx, |f, cx| f.set_text(&text, window, cx));
                    }
                }
                self.reset_test();
                cx.notify();
            }
            self.last_seen = ai;
        }

        fn build_fields(
            &mut self,
            provider: Option<AiProvider>,
            ai: &AiSettings,
            window: &mut Window,
            cx: &mut Context<Self>,
        ) {
            let Some(provider) = provider else {
                self.fields = None;
                return;
            };
            let model_hint = backend_ai::default_model(provider);
            let endpoint_hint = backend_ai::endpoint_hint(provider);
            let model = cx.new(|cx| {
                OutlinedField::new(settings_ids::field(ids::MODEL), window, cx)
                    .hint(model_hint, window, cx)
            });
            let endpoint = cx.new(|cx| {
                OutlinedField::new(settings_ids::field(ids::ENDPOINT), window, cx).hint(
                    endpoint_hint,
                    window,
                    cx,
                )
            });
            let model_text = backend_ai::model_of(ai, provider).to_owned();
            let endpoint_text = backend_ai::endpoint_of(ai, provider);
            model.update(cx, |f, cx| f.set_text(&model_text, window, cx));
            endpoint.update(cx, |f, cx| f.set_text(&endpoint_text, window, cx));
            let subscriptions = vec![
                cx.subscribe_in(&model, window, |this, field, event: &FieldEvent, _, cx| {
                    if *event == FieldEvent::Changed {
                        this.model_changed(field.read(cx).text(cx).to_string(), cx);
                    }
                }),
                cx.subscribe_in(
                    &endpoint,
                    window,
                    |this, field, event: &FieldEvent, _, cx| {
                        if *event == FieldEvent::Changed {
                            this.endpoint_changed(field.read(cx).text(cx).to_string(), cx);
                        }
                    },
                ),
            ];
            self.fields = Some(ProviderFields {
                model,
                endpoint,
                _subscriptions: subscriptions,
            });
        }

        /// Writes `edit` of the AI settings (the value is stored as the opaque `ai` setting).
        fn edit_ai(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut AiSettings)) {
            let mut ai = current_ai(cx);
            edit(&mut ai);
            let value = backend_ai::settings_value(&ai);
            settings::update(cx, |s| s.ai = value);
        }

        fn model_changed(&mut self, text: String, cx: &mut Context<Self>) {
            let Some(provider) = self.shown_provider else {
                return;
            };
            match backend_ai::validate_model(provider, &text) {
                Ok(()) => {
                    self.model_error = None;
                    let text = text.trim().to_owned();
                    self.edit_ai(cx, |ai| backend_ai::set_model(ai, provider, &text));
                }
                Err(message) => self.model_error = Some(message),
            }
            self.after_edit(cx);
        }

        fn endpoint_changed(&mut self, text: String, cx: &mut Context<Self>) {
            let Some(provider) = self.shown_provider else {
                return;
            };
            match backend_ai::validate_endpoint(provider, &text) {
                Ok(()) => {
                    self.endpoint_error = None;
                    self.edit_ai(cx, |ai| backend_ai::set_endpoint(ai, provider, &text));
                }
                Err(message) => self.endpoint_error = Some(message),
            }
            self.after_edit(cx);
        }

        fn after_edit(&mut self, cx: &mut Context<Self>) {
            if let Some(fields) = &self.fields {
                let (model_bad, endpoint_bad) =
                    (self.model_error.is_some(), self.endpoint_error.is_some());
                fields.model.update(cx, |f, cx| f.set_error(model_bad, cx));
                fields
                    .endpoint
                    .update(cx, |f, cx| f.set_error(endpoint_bad, cx));
            }
            self.reset_test();
            cx.notify();
        }

        // ── the API key ──────────────────────────────────────────────────────────────────────

        fn handle(&self) -> Option<&'static str> {
            AiSettings::secret_handle(self.shown_provider?)
        }

        /// Asks the key store (off the UI thread) whether the selected provider has a key.
        pub fn probe_key(&mut self, cx: &mut Context<Self>) {
            let Some(handle) = self.handle() else {
                self.key_state = None;
                return;
            };
            let keys = ai::key_store(cx);
            let task = crate::backend::bg(cx, move || keys.state(handle));
            let provider = self.shown_provider;
            cx.spawn(async move |this, cx| {
                let state = task.await;
                this.update(cx, |page, cx| {
                    if page.shown_provider == provider {
                        page.key_state = Some(state);
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }

        fn key_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
            if self.key.read(cx).text(cx).is_empty() {
                self.key_task = None;
                return;
            }
            self.key_task = Some(cx.spawn_in(window, async move |this, cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(KEY_DEBOUNCE_MS))
                    .await;
                this.update_in(cx, |page, window, cx| {
                    page.key_task = None;
                    page.store_key(window, cx);
                })
                .ok();
            }));
        }

        /// Stores what the key field holds under the selected provider's handle and clears the field (the key is
        /// write-only).
        pub fn store_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
            self.key_task = None;
            let Some(handle) = self.handle() else {
                return;
            };
            let key = self.key.read(cx).text(cx).trim().to_owned();
            if key.is_empty() {
                return;
            }
            let keys = ai::key_store(cx);
            let stored = key.clone();
            let task = crate::backend::bg(cx, move || keys.store(handle, &key));
            cx.spawn_in(window, async move |this, cx| {
                let result = task.await;
                this.update_in(cx, |page, window, cx| {
                    page.key_written(result, Some(stored), window, cx)
                })
                .ok();
            })
            .detach();
        }

        /// Removes the stored key of the selected provider.
        pub fn remove_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
            let Some(handle) = self.handle() else {
                return;
            };
            let keys = ai::key_store(cx);
            let task = crate::backend::bg(cx, move || keys.delete(handle));
            cx.spawn_in(window, async move |this, cx| {
                let result = task.await;
                this.update_in(cx, |page, window, cx| {
                    page.key_written(result, None, window, cx)
                })
                .ok();
            })
            .detach();
        }

        /// A store (`stored` = the key written) or a removal (`None`) finished.
        fn key_written(
            &mut self,
            result: Result<(), String>,
            stored: Option<String>,
            window: &mut Window,
            cx: &mut Context<Self>,
        ) {
            match result {
                Ok(()) => {
                    // The field is write-only: what was typed is gone once it is stored. Text typed while the
                    // keychain was being written is kept (its own store is scheduled).
                    let field = self.key.read(cx).text(cx).trim().to_owned();
                    if stored.is_some_and(|stored| stored == field) {
                        self.key.update(cx, |f, cx| f.set_text("", window, cx));
                    }
                    self.reset_test();
                    self.probe_key(cx);
                    ai::keys_changed(cx);
                }
                // The message of the keychain error never contains the key.
                Err(err) => crate::app_view::show_toast(
                    cx,
                    None,
                    ToastKind::Error,
                    format!("Cannot update the API key: {err}"),
                    None,
                ),
            }
            cx.notify();
        }

        // ── test connection ──────────────────────────────────────────────────────────────────

        /// Forgets the last connection test and withdraws a running one (the settings it tested changed).
        fn reset_test(&mut self) {
            if let Some(running) = self.test_cancel.take() {
                running.cancel();
            }
            self.test_run += 1;
            self.test = TestState::Idle;
        }

        /// Applies the current settings and sends one tiny request to the provider.
        pub fn test_connection(&mut self, cx: &mut Context<Self>) {
            if self.test == TestState::Running {
                return;
            }
            let Some(services) = crate::app::services(cx) else {
                self.test = TestState::Failed("The integrations did not start".into());
                cx.notify();
                return;
            };
            if let Some(refusal) = ai::offline_refusal(cx) {
                self.test = TestState::Failed(refusal);
                cx.notify();
                return;
            }
            self.reset_test();
            let cancel = CancelToken::new();
            self.test_cancel = Some(cancel.clone());
            self.test = TestState::Running;
            let run = self.test_run;
            let ai = current_ai(cx);
            let keys = ai::key_store(cx);
            let task = crate::backend::bg(cx, move || {
                backend_ai::test_connection(&services, &ai, &keys, &cancel)
            });
            cx.spawn(async move |this, cx| {
                let result = task.await;
                this.update(cx, |page, cx| {
                    // The settings changed while it ran: the answer is about settings no longer in effect.
                    if page.test_run != run {
                        return;
                    }
                    page.test_cancel = None;
                    page.test = match result {
                        Ok(()) => TestState::Ok,
                        Err(backend_ai::AiFailure::Cancelled) => TestState::Idle,
                        Err(failure) => TestState::Failed(failure.message()),
                    };
                    cx.notify();
                })
                .ok();
            })
            .detach();
            cx.notify();
        }

        // ── rendering ────────────────────────────────────────────────────────────────────────

        /// A dropdown (the look of the other pages' dropdowns: 180 dp box, the popup drawn from inside the dialog,
        /// over it).
        fn select(
            &mut self,
            key: &'static str,
            current: impl Into<SharedString>,
            options: Vec<(SharedString, Pick)>,
            cx: &mut Context<Self>,
        ) -> AnyElement {
            let tokens = *cx.tokens();
            let bounds = self
                .select_bounds
                .entry(key)
                .or_insert_with(|| Rc::new(Cell::new(Default::default())))
                .clone();
            let open = self.open_select == Some(key);
            let trigger = div()
                .id(settings_ids::select(key))
                .w_full()
                .flex()
                .flex_row()
                .items_center()
                .py(dp(6.))
                .px(dp(10.))
                .rounded(dp(metrics::RADIUS))
                .border(dp(2.))
                .border_color(tokens.dialog_border())
                .bg(tokens.background)
                .cursor_pointer()
                .hover(move |s| s.bg(with_layer(tokens.background, tokens.hover_layer())))
                .child(
                    div()
                        .flex_1()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_body2()
                        .text_color(tokens.on_background)
                        .child(current.into()),
                )
                .child(icon(AppIcon::ExpandMore, 24., tokens.on_background))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.open_select = if this.open_select == Some(key) {
                        None
                    } else {
                        Some(key)
                    };
                    cx.notify();
                }))
                .test_support();
            let popup = open.then(|| {
                let rows: Vec<AnyElement> = options
                    .into_iter()
                    .enumerate()
                    .map(|(index, (label, pick))| {
                        div()
                            .id(settings_ids::select_option(key, index))
                            .w_full()
                            .px(dp(12.))
                            .py(dp(metrics::MENU_ITEM_PAD))
                            .text_body2()
                            .text_color(tokens.on_background)
                            .hover(move |s| s.bg(tokens.hover_layer()))
                            .active(move |s| s.bg(tokens.press_layer()))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_select = None;
                                pick(this, window, cx);
                                cx.notify();
                            }))
                            .child(label)
                            .test_support()
                            .into_any_element()
                    })
                    .collect();
                let card = observed(settings_ids::select_popup(key))
                    .min_w(dp(metrics::MENU_MIN_W))
                    .flex()
                    .flex_col()
                    .rounded(dp(metrics::RADIUS))
                    .bg(tokens.background)
                    .overflow_hidden()
                    .shadow(vec![BoxShadow {
                        color: tokens.menu_shadow(),
                        offset: point(px(0.), px(2.)),
                        blur_radius: px(5.),
                        spread_radius: px(1.),
                        inset: false,
                    }])
                    .when(!tokens.is_light, |m| {
                        m.border(dp(2.)).border_color(tokens.popup_border())
                    })
                    .children(rows);
                let at = bounds.get().bottom_left();
                let viewport = self.viewport;
                deferred(
                    anchored().position(point(px(0.), px(0.))).child(
                        div()
                            .w(viewport.width)
                            .h(viewport.height)
                            .occlude()
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                                    this.open_select = None;
                                    cx.notify();
                                }),
                            )
                            .child(
                                anchored().position(at).snap_to_window().child(
                                    div()
                                        .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                            cx.stop_propagation()
                                        })
                                        .child(card),
                                ),
                            ),
                    ),
                )
                .with_priority(POPUP_PRIORITY)
            });
            div()
                .relative()
                .flex_none()
                .w(dp(sm::DROPDOWN_W))
                .child(track_bounds(&bounds))
                .child(trigger)
                .children(popup)
                .into_any_element()
        }

        fn provider_row(&mut self, ai: &AiSettings, cx: &mut Context<Self>) -> AnyElement {
            let current = ai.provider.map_or("None", |p| p.display_name());
            let mut options: Vec<(SharedString, Pick)> = vec![(
                "None".into(),
                Rc::new(|this: &mut AiPage, window, cx| this.pick_provider(None, window, cx)),
            )];
            for provider in backend_ai::PROVIDERS {
                options.push((
                    provider.display_name().into(),
                    Rc::new(move |this: &mut AiPage, window, cx| {
                        this.pick_provider(Some(provider), window, cx)
                    }),
                ));
            }
            SettingRow::new(ids::PROVIDER, "Provider")
                .subtitle(
                    "The service that writes commit messages. Hosted providers receive your staged changes \
                     (you are asked once); Ollama and LM Studio run on your machine.",
                )
                .control(self.select(ids::PROVIDER, current, options, cx))
                .into_any_element()
        }

        fn pick_provider(
            &mut self,
            provider: Option<AiProvider>,
            window: &mut Window,
            cx: &mut Context<Self>,
        ) {
            self.edit_ai(cx, |ai| ai.provider = provider);
            self.sync(window, cx);
        }

        fn style_row(&mut self, cx: &mut Context<Self>) -> AnyElement {
            let style = settings::get_or_default(cx).ai_commit_style;
            let label = match style {
                AiCommitStyle::Conventional => "Conventional",
                AiCommitStyle::PlainImperative => "Plain imperative",
            };
            let options: Vec<(SharedString, Pick)> = [
                ("Conventional", AiCommitStyle::Conventional),
                ("Plain imperative", AiCommitStyle::PlainImperative),
            ]
            .into_iter()
            .map(|(label, style)| {
                let pick: Pick = Rc::new(move |_: &mut AiPage, _, cx| {
                    settings::update(cx, |s| s.ai_commit_style = style)
                });
                (SharedString::from(label), pick)
            })
            .collect();
            SettingRow::new(ids::STYLE, "Commit message style")
                .subtitle("Conventional: a type prefix such as feat: or fix:. Plain imperative: a short imperative subject.")
                .control(self.select(ids::STYLE, label, options, cx))
                .into_any_element()
        }

        fn key_row(&mut self, provider: AiProvider, cx: &mut Context<Self>) -> AnyElement {
            let tokens = *cx.tokens();
            let variable = AiSettings::secret_handle(provider)
                .and_then(backend_ai::environment_variable)
                .map(|name| format!(" Or set {name}."))
                .unwrap_or_default();
            let mut row = SettingRow::new(ids::KEY, "API key").subtitle(format!(
                "Write-only: the key is kept in your system keychain, never in the settings file.{variable}"
            ));
            match &self.key_state {
                Some(KeyState::Environment(name)) => {
                    row = row.note(
                        format!("Using the key of the {name} environment variable. A key stored here takes precedence."),
                        false,
                    );
                }
                Some(KeyState::Unavailable(reason)) => {
                    row = row.note(format!("The keychain cannot be used: {reason}"), true);
                }
                _ => {}
            }
            let stored = matches!(self.key_state, Some(KeyState::Stored));
            let remove_this = cx.weak_entity();
            let control = div()
                .flex()
                .flex_row()
                .items_center()
                .gap(dp(ai::metrics::ROW_CONTROL_GAP))
                .child(div().w(dp(sm::TEXT_FIELD_W)).child(self.key.clone()))
                .when(stored, |c| {
                    c.child(
                        observed(ids::KEY_STORED)
                            .flex_none()
                            .px(dp(ai::metrics::BADGE_PAD_H))
                            .py(dp(ai::metrics::BADGE_PAD_V))
                            .rounded(dp(metrics::RADIUS))
                            .bg(tokens.background_selected)
                            .text_body2()
                            .text_color(tokens.on_background)
                            .child("Stored"),
                    )
                    .child(
                        SecondaryButton::new(ids::KEY_REMOVE, "Remove").on_click(
                            move |window, cx| {
                                remove_this
                                    .update(cx, |page, cx| page.remove_key(window, cx))
                                    .ok();
                            },
                        ),
                    )
                });
            row.control(control).into_any_element()
        }

        fn status_row(&mut self, cx: &mut Context<Self>) -> AnyElement {
            let readiness = ai::readiness(cx);
            let text = match &readiness {
                Readiness::Ready { provider, model } => format!("Ready: {provider} {model}"),
                Readiness::Off => "Not configured: no provider selected".to_owned(),
                Readiness::NotConfigured(missing) => {
                    format!("Not configured: {}", backend_ai::missing_text(*missing))
                }
                Readiness::Pending => "Checking...".to_owned(),
            };
            let mut row = SettingRow::new(ids::STATUS, "Status").subtitle(text);
            match &self.test {
                TestState::Idle => {}
                TestState::Running => row = row.note("Testing the connection...", false),
                TestState::Ok => row = row.note("The provider answered.", false),
                TestState::Failed(reason) => {
                    row = row.note(format!("Connection test failed: {reason}"), true)
                }
            }
            let this = cx.weak_entity();
            row.control(SecondaryButton::new(ids::TEST, "Test connection").on_click(
                move |_, cx| {
                    this.update(cx, |page, cx| page.test_connection(cx)).ok();
                },
            ))
            .into_any_element()
        }

        fn field_row(
            key: &'static str,
            title: &'static str,
            subtitle: String,
            field: Entity<OutlinedField>,
            error: Option<&str>,
        ) -> AnyElement {
            let mut row = SettingRow::new(key, title).subtitle(subtitle);
            if let Some(error) = error {
                row = row.note(error.to_owned(), true);
            }
            row.control(div().w(dp(sm::TEXT_FIELD_W)).child(field))
                .into_any_element()
        }
    }

    impl Render for AiPage {
        fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            self.viewport = window.viewport_size();
            let ai = current_ai(cx);
            let mut rows = vec![self.provider_row(&ai, cx)];
            if let (Some(provider), Some((model, endpoint))) = (ai.provider, self.provider_fields())
            {
                rows.push(Self::field_row(
                    ids::MODEL,
                    "Model",
                    format!(
                        "The model name to use, for example {}",
                        backend_ai::default_model(provider)
                    ),
                    model,
                    self.model_error.as_deref(),
                ));
                if AiSettings::secret_handle(provider).is_some() {
                    rows.push(self.key_row(provider, cx));
                }
                if backend_ai::has_endpoint(provider) {
                    let subtitle = match provider {
                        AiProvider::OpenAi => {
                            "Optional base URL of an OpenAI-compatible server; leave empty for OpenAI"
                        }
                        AiProvider::Ollama => "Address of the Ollama server as host:port",
                        _ => "Base URL of the server",
                    };
                    rows.push(Self::field_row(
                        ids::ENDPOINT,
                        "Endpoint",
                        subtitle.to_owned(),
                        endpoint,
                        self.endpoint_error.as_deref(),
                    ));
                }
            }
            rows.push(self.style_row(cx));
            rows.push(self.status_row(cx));
            observed("settings-ai-page")
                .w_full()
                .flex()
                .flex_col()
                .children(rows)
        }
    }
}
