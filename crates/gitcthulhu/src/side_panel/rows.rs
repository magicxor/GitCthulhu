//! Row rendering of the side panel per row kind (owned by G3 after G0b; 07 §6.3-§6.5). Local-branch rows append
//! `forge::badges::branch_row_badge(..)` before "HEAD"; worktree rows render through `worktrees::render_row`.
//!
//! Every row is a fixed-height `div` (36 dp spaced, 34 compact) with a domain id; handlers capture the row's own
//! data (ref names, ids), never an index into the model, so a reload between paint and click cannot misdirect them.

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClickEvent, Context, ElementId, InteractiveElement as _, IntoElement,
    MouseButton, MouseDownEvent, ParentElement as _, SharedString, StatefulInteractiveElement as _,
    Styled as _, TestSupportExt as _, Window, div,
};

use super::SidePanelView;
use super::ids;
use super::model::{Section, SideRow, stash_title, submodule_is_valid, submodule_state};
use crate::backend::{
    AheadBehind, ByteSlice as _, FullName, LocalBranch, ObjectId, RemoteBranch, RemoteInfo,
    StashInfo, SubmoduleInfo, TagInfo,
};
use crate::icons::{AppIcon, icon};
use crate::menus::{self, MenuItem, MenuTarget, refs};
use crate::metrics::side_panel as m;
use crate::metrics::{self, dp};
use crate::tab::RepoTabEvent;
use crate::tab::ui::{RepoUiEvent, Selection};
use crate::theme::{ActiveTokens as _, TextStyleExt as _, Tokens};
use crate::widgets::TooltipExt as _;

type MenuFn = Box<dyn Fn(&MouseDownEvent, &mut Window, &mut App)>;

/// Everything one entry row (branch, remote, tag, stash, submodule) is made of.
struct Entry {
    id: SharedString,
    role: AppIcon,
    text: SharedString,
    bold: bool,
    selected: bool,
    extra_pad: f32,
    trailing: Vec<AnyElement>,
}

/// An entry row (07 §6.4): full-width hover layer, selection background after `extra_pad`, 16 dp icon at
/// `32 + extra_pad`, clipped one-line text, then the trailing content.
fn entry(
    e: Entry,
    height: f32,
    tokens: &Tokens,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    on_menu: MenuFn,
) -> AnyElement {
    let tokens = *tokens;
    let id: ElementId = e.id.into();
    // The hover/press layer lies over the selection (Compose draws the indication on top of the background). A
    // selected row without extra padding (every selectable kind; remote branches are never selected) paints its
    // selection on the row itself, with the layers blended onto it; with extra padding the inner box paints it.
    let outer_selected = e.selected && e.extra_pad == 0.;
    let inner_selected = e.selected && e.extra_pad > 0.;
    let base = outer_selected.then_some(tokens.background_selected);
    let layer = move |l: gpui_kit::Hsla| base.map_or(l, |b| b.blend(l));
    div()
        .id(id.clone())
        .w_full()
        .h(dp(height))
        .flex_none()
        .pl(dp(e.extra_pad))
        .when_some(base, |row, bg| row.bg(bg))
        .hover(move |s| s.bg(layer(tokens.hover_layer())))
        .active(move |s| s.bg(layer(tokens.press_layer())))
        .on_click(on_click)
        .on_mouse_down(MouseButton::Right, move |ev, window, cx| {
            on_menu(ev, window, cx)
        })
        .child(
            div()
                .size_full()
                .flex()
                .flex_row()
                .items_center()
                .when(inner_selected, |row| row.bg(tokens.background_selected))
                .child(
                    div()
                        .flex_none()
                        .pl(dp(m::ROW_ICON_PAD_START))
                        .pr(dp(m::ROW_ICON_PAD_END))
                        .child(
                            div()
                                .id((id.clone(), "icon"))
                                .size(dp(m::ICON))
                                .child(icon(e.role, m::ICON, tokens.primary_variant))
                                .test_support(),
                        ),
                )
                .child(
                    div()
                        .id((id, "text"))
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .text_body2()
                        .text_color(tokens.on_background)
                        .when(e.bold, |t| t.bold())
                        .child(e.text)
                        .test_support(),
                )
                .children(e.trailing),
        )
        .test_support()
        .into_any_element()
}

/// A blank row of the right height (a row whose data vanished between the build and the paint).
fn blank(height: f32) -> AnyElement {
    div().w_full().h(dp(height)).flex_none().into_any_element()
}

/// The caption text of a trailing "HEAD" badge or state letter.
fn trailing_text(text: impl Into<SharedString>, tokens: &Tokens) -> gpui_kit::Div {
    div()
        .flex_none()
        .px(dp(m::ROW_TRAILING_PAD_H))
        .text_color(tokens.on_background_secondary)
        .child(text.into())
}

/// "↑2 ↓1" for the commits a branch is ahead of / behind its upstream; `None` when it is level.
pub(crate) fn ahead_behind_text(ab: &AheadBehind) -> Option<String> {
    let plus = if ab.capped { "+" } else { "" };
    let mut parts = Vec::new();
    if ab.ahead > 0 {
        parts.push(format!("↑{}{plus}", ab.ahead));
    }
    if ab.behind > 0 {
        parts.push(format!("↓{}{plus}", ab.behind));
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

impl SidePanelView {
    /// One row of the list.
    pub(super) fn render_row(
        &mut self,
        row: &SideRow,
        wanted: &mut Vec<(ObjectId, ObjectId)>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = *cx.tokens();
        let height = metrics::line_heights(cx).side_item;
        if let SideRow::Header {
            section,
            count,
            expanded,
        } = *row
        {
            return self.header_row(section, count, expanded, height, &tokens, cx);
        }
        let (refs_value, subs, worktrees, ahead_behind) = {
            let model = self.ctx.model.read(cx);
            (
                model.refs.value.clone(),
                model.submodules.value.clone(),
                model.worktrees.value.clone(),
                model.ahead_behind.clone(),
            )
        };
        let selection = self.ctx.ui.read(cx).selection.clone();
        let local = |ix: usize| refs_value.as_ref().and_then(|r| r.local.get(ix));
        match *row {
            SideRow::Header { .. } => blank(height),
            SideRow::LocalBranch { ix } => {
                let Some(branch) = local(ix) else {
                    return blank(height);
                };
                let pair = self.built.pairs.get(&ix).copied();
                if let Some(pair) = pair {
                    wanted.push(pair);
                }
                let ab = pair
                    .and_then(|p| ahead_behind.get(&p))
                    .and_then(ahead_behind_text);
                self.local_branch_row(branch, ab, &selection, height, &tokens, cx)
            }
            SideRow::Remote { ix, .. } => {
                let Some(remote) = refs_value.as_ref().and_then(|r| r.remotes.get(ix)) else {
                    return blank(height);
                };
                self.remote_row(remote, height, &tokens, cx)
            }
            SideRow::RemoteBranch { remote, ix } => {
                let Some(branch) = refs_value
                    .as_ref()
                    .and_then(|r| r.remotes.get(remote))
                    .and_then(|r| r.branches.get(ix))
                else {
                    return blank(height);
                };
                self.remote_branch_row(branch, height, &tokens, cx)
            }
            SideRow::Tag { ix } => {
                let Some(tag) = refs_value.as_ref().and_then(|r| r.tags.get(ix)) else {
                    return blank(height);
                };
                self.tag_row(tag, &selection, height, &tokens, cx)
            }
            SideRow::Stash { ix } => {
                let Some(stash) = refs_value.as_ref().and_then(|r| r.stashes.get(ix)) else {
                    return blank(height);
                };
                self.stash_row(stash, &selection, height, &tokens, cx)
            }
            SideRow::Submodule { ix } => {
                let Some(sub) = subs.as_ref().and_then(|s| s.get(ix)) else {
                    return blank(height);
                };
                self.submodule_row(sub, height, &tokens, cx)
            }
            SideRow::Worktree { ix } => match worktrees.as_ref().and_then(|w| w.get(ix)) {
                Some(info) => crate::worktrees::render_row(&self.ctx, info, false, cx),
                None => blank(height),
            },
        }
    }

    /// A section header (07 §6.3): chevron, section icon, title, then the count (or "+" while hovered for the
    /// sections that have an add action).
    fn header_row(
        &self,
        section: Section,
        count: usize,
        expanded: bool,
        height: f32,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = *tokens;
        let role = match section {
            Section::LocalBranches => AppIcon::Branch,
            Section::Remotes => AppIcon::Cloud,
            Section::Tags => AppIcon::Tag,
            Section::Stashes => AppIcon::Stash,
            Section::Submodules => AppIcon::Submodule,
            Section::Worktrees => AppIcon::Worktree,
        };
        // The count and the "+" swap by visibility, never by `display`: GPUI decides `display: none` separately in
        // prepaint and paint, and a header that moves under a resting pointer (a refresh removed a row) flips its
        // group hover in between, which paints children that were never prepainted (a panic).
        let count_text = div()
            .flex_none()
            .pr(dp(m::HEADER_TRAILING_PAD_END))
            .text_body2()
            .medium()
            .text_color(tokens.on_background_secondary)
            .when(section.has_add_button(), |c| {
                c.group_hover("side-header", |s| s.invisible())
            })
            .child(SharedString::from(count.to_string()));
        div()
            .id(ids::header(section))
            .group("side-header")
            .w_full()
            .h(dp(height))
            .flex_none()
            .flex()
            .flex_row()
            .items_center()
            .cursor_pointer()
            .hover(move |s| s.bg(tokens.hover_layer()))
            .on_click(
                cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_section(section, cx)),
            )
            .when(section == Section::Worktrees, |header| {
                // D0.7 (X6): right-click on the Worktrees header opens its menu (Add worktree, Prune worktrees).
                let target = menus::worktree::header_target(&self.ctx);
                header.on_mouse_down(MouseButton::Right, move |ev, window, cx| {
                    let items = menus::worktree::worktrees_header(&target, cx);
                    menus::open_context_menu(window, cx, ev.position, items);
                })
            })
            .child(
                div()
                    .flex_none()
                    .px(dp(m::HEADER_CHEVRON_PAD_H))
                    .child(icon(
                        if expanded {
                            AppIcon::ExpandMore
                        } else {
                            AppIcon::ChevronRight
                        },
                        m::ICON,
                        tokens.on_background,
                    )),
            )
            .child(
                div()
                    .id(ids::child(ids::header(section), "icon"))
                    .flex_none()
                    .size(dp(m::ICON))
                    .child(icon(role, m::ICON, tokens.on_background))
                    .test_support(),
            )
            .child(
                div()
                    .id(ids::child(ids::header(section), "title"))
                    .flex_1()
                    .min_w_0()
                    .px(dp(m::HEADER_TITLE_PAD_H))
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_body2()
                    .medium()
                    .text_color(tokens.on_background)
                    .child(section.title())
                    .test_support(),
            )
            .child(
                div()
                    .relative()
                    .flex_none()
                    .h_full()
                    .flex()
                    .items_center()
                    .child(count_text)
                    .when(section.has_add_button(), |trailing| {
                        trailing.child(
                            div()
                                .id(ids::header_add(section))
                                .absolute()
                                .top_0()
                                .right_0()
                                .h_full()
                                .flex()
                                .items_center()
                                .pr(dp(m::HEADER_TRAILING_PAD_END))
                                .invisible()
                                .group_hover("side-header", |s| s.visible())
                                .cursor_pointer()
                                .child(icon(AppIcon::Add, m::ICON, tokens.on_background))
                                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                    cx.stop_propagation();
                                    this.on_header_add(section, window, cx);
                                }))
                                .test_support(),
                        )
                    }),
            )
            .test_support()
            .into_any_element()
    }

    fn local_branch_row(
        &self,
        branch: &LocalBranch,
        ahead_behind: Option<String>,
        selection: &Selection,
        height: f32,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let full_text = branch.full.as_bstr().to_str_lossy().into_owned();
        let selected = matches!(selection, Selection::Branch { full, .. } if *full == branch.full);
        let mut trailing = Vec::new();
        if let Some(badge) = crate::forge::badges::branch_row_badge(&self.ctx, &branch.full, cx) {
            trailing.push(badge);
        }
        if let Some(text) = ahead_behind {
            let upstream = branch
                .upstream
                .as_ref()
                .and_then(|u| u.tracking.as_ref())
                .map(|t| t.shorten().to_str_lossy().into_owned())
                .unwrap_or_default();
            trailing.push(
                div()
                    .id(ids::child(ids::branch(&full_text), "ahead-behind"))
                    .flex_none()
                    .pl(dp(m::ROW_TRAILING_GAP))
                    .pr(dp(if branch.is_head {
                        0.
                    } else {
                        m::ROW_TRAILING_PAD_H
                    }))
                    .text_caption()
                    .text_color(tokens.on_background_secondary)
                    .delayed_tooltip(format!("Ahead and behind {upstream}"))
                    .child(SharedString::from(text))
                    .test_support()
                    .into_any_element(),
            );
        }
        if branch.is_head {
            trailing.push(
                trailing_text("HEAD", tokens)
                    .id(ids::child(ids::branch(&full_text), "head"))
                    .text_caption()
                    .semibold()
                    .test_support()
                    .into_any_element(),
            );
        }
        let (full, target, current) = (branch.full.clone(), branch.target, branch.is_head);
        let menu_target = refs::local_branch_target(&self.ctx, branch);
        entry(
            Entry {
                id: ids::branch(&full_text),
                role: AppIcon::Branch,
                text: SharedString::from(branch.name.clone()),
                bold: branch.is_head,
                selected,
                extra_pad: 0.,
                trailing,
            },
            height,
            tokens,
            cx.listener(move |this, ev: &ClickEvent, window, cx| {
                this.on_branch_click(&full, target, current, ev.click_count(), window, cx);
            }),
            context_menu(menu_target, refs::local_branch),
        )
    }

    fn remote_row(
        &self,
        remote: &RemoteInfo,
        height: f32,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let name = remote.name.clone();
        let menu_target = refs::remote_target(&self.ctx, &remote.name);
        entry(
            Entry {
                id: ids::remote(&remote.name),
                role: AppIcon::Cloud,
                text: SharedString::from(remote.name.clone()),
                bold: false,
                selected: false,
                extra_pad: 0.,
                trailing: Vec::new(),
            },
            height,
            tokens,
            cx.listener(move |this, _: &ClickEvent, _, cx| this.toggle_remote(&name, cx)),
            context_menu(menu_target, refs::remote),
        )
    }

    fn remote_branch_row(
        &self,
        branch: &RemoteBranch,
        height: f32,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let full_text = branch.full.as_bstr().to_str_lossy().into_owned();
        let (full, target) = (branch.full.clone(), branch.target);
        let menu_target = refs::remote_branch_target(&self.ctx, branch);
        entry(
            Entry {
                id: ids::remote_branch(&full_text),
                role: AppIcon::Branch,
                text: SharedString::from(branch.name.clone()),
                bold: false,
                selected: false,
                extra_pad: m::REMOTE_BRANCH_EXTRA_PAD,
                trailing: Vec::new(),
            },
            height,
            tokens,
            cx.listener(move |this, ev: &ClickEvent, window, cx| {
                this.on_remote_branch_click(&full, target, ev.click_count(), window, cx);
            }),
            context_menu(menu_target, refs::remote_branch),
        )
    }

    fn tag_row(
        &self,
        tag: &TagInfo,
        selection: &Selection,
        height: f32,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let full_text = tag.full.as_bstr().to_str_lossy().into_owned();
        let selected = matches!(selection, Selection::Tag { full, .. } if *full == tag.full);
        let (full, target) = (tag.full.clone(), tag.target);
        let menu_target = refs::tag_target(&self.ctx, tag.full.clone(), tag.target);
        entry(
            Entry {
                id: ids::tag(&full_text),
                role: AppIcon::Tag,
                text: SharedString::from(tag.name.clone()),
                bold: false,
                selected,
                extra_pad: 0.,
                trailing: Vec::new(),
            },
            height,
            tokens,
            cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.select(
                    Selection::Tag {
                        full: full.clone(),
                        target,
                    },
                    target,
                    cx,
                );
            }),
            context_menu(menu_target, refs::tag),
        )
    }

    fn stash_row(
        &self,
        stash: &StashInfo,
        selection: &Selection,
        height: f32,
        tokens: &Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = matches!(selection, Selection::Stash { id, .. } if *id == stash.id);
        let (id, index) = (stash.id, stash.index);
        let menu_target = refs::stash_target(&self.ctx, stash);
        entry(
            Entry {
                id: ids::stash(&stash.id.to_hex().to_string()),
                role: AppIcon::Stash,
                text: SharedString::from(stash_title(&stash.message).to_owned()),
                bold: false,
                selected,
                extra_pad: 0.,
                trailing: Vec::new(),
            },
            height,
            tokens,
            cx.listener(move |this, _: &ClickEvent, _, cx| {
                this.select(Selection::Stash { id, index }, id, cx);
            }),
            context_menu(menu_target, refs::stash),
        )
    }

    fn submodule_row(
        &self,
        sub: &SubmoduleInfo,
        height: f32,
        tokens: &Tokens,
        _cx: &mut Context<Self>,
    ) -> AnyElement {
        let path_text = sub.path.to_str_lossy().into_owned();
        let (letter, state_name) = submodule_state(sub.state);
        let valid = submodule_is_valid(sub.state);
        let path = sub.path.clone();
        let ctx = self.ctx.clone();
        let menu_target = refs::submodule_target(&self.ctx, sub);
        let letter_el = trailing_text(letter, tokens)
            .id(ids::child(ids::submodule(&path_text), "state"))
            .text_body2()
            .delayed_tooltip(state_name)
            .test_support()
            .into_any_element();
        entry(
            Entry {
                id: ids::submodule(&path_text),
                role: AppIcon::Submodule,
                text: SharedString::from(path_text),
                bold: false,
                selected: false,
                extra_pad: 0.,
                trailing: vec![letter_el],
            },
            height,
            tokens,
            move |_, window, cx| {
                if valid {
                    refs::open_submodule_in_tab(&ctx, &path, window, cx);
                }
            },
            context_menu(menu_target, refs::submodule),
        )
    }

    // ---- behaviour -------------------------------------------------------------------------------------------

    /// Selects `selection` and asks the log to bring `commit` into view when it is not visible.
    pub(super) fn select(
        &mut self,
        selection: Selection,
        commit: ObjectId,
        cx: &mut Context<Self>,
    ) {
        self.ctx.ui.update(cx, |ui, cx| ui.select(selection, cx));
        self.ctx.events.update(cx, |_, cx| {
            cx.emit(RepoTabEvent::ScrollToCommit { id: commit });
        });
    }

    /// Click selects the branch's commit; a double-click also checks the branch out (not the current one).
    fn on_branch_click(
        &mut self,
        full: &FullName,
        target: ObjectId,
        current: bool,
        click_count: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select(
            Selection::Branch {
                full: full.clone(),
                target,
            },
            target,
            cx,
        );
        if click_count == 2 && !current {
            let full = full.clone();
            self.ctx.with_tab(window, cx, move |tab, window, cx| {
                tab.checkout_branch(full, window, cx);
            });
        }
    }

    /// Click selects the commit; a double-click checks the remote branch out as a tracking branch.
    fn on_remote_branch_click(
        &mut self,
        full: &FullName,
        target: ObjectId,
        click_count: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.select(
            Selection::Branch {
                full: full.clone(),
                target,
            },
            target,
            cx,
        );
        if click_count == 2 {
            let full = full.clone();
            self.ctx.with_tab(window, cx, move |tab, window, cx| {
                tab.checkout_remote_branch(full, None, window, cx);
            });
        }
    }

    /// Expands or collapses a section (held per tab, not persisted).
    pub(super) fn toggle_section(&mut self, section: Section, cx: &mut Context<Self>) {
        self.ctx.ui.update(cx, |ui, cx| {
            section.toggle(&mut ui.side_expanded);
            cx.emit(RepoUiEvent::TogglesChanged);
            cx.notify();
        });
        cx.notify();
    }

    /// Collapses or expands the branch list of one remote.
    pub(super) fn toggle_remote(&mut self, name: &str, cx: &mut Context<Self>) {
        let name = name.to_owned();
        self.ctx.ui.update(cx, |ui, cx| {
            if !ui.remotes_collapsed.remove(&name) {
                ui.remotes_collapsed.insert(name);
            }
            cx.emit(RepoUiEvent::TogglesChanged);
            cx.notify();
        });
        cx.notify();
    }

    /// The "+" of a header: the New remote / New submodule dialog.
    pub(super) fn on_header_add(
        &mut self,
        section: Section,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let ctx = self.ctx.clone();
        match section {
            Section::Remotes => crate::dialogs::remote::open_new_remote(window, cx, ctx),
            Section::Submodules => crate::dialogs::submodule::open_new_submodule(window, cx, ctx),
            Section::Worktrees => crate::dialogs::worktree::open_add_worktree(window, cx, ctx),
            Section::LocalBranches | Section::Tags | Section::Stashes => {}
        }
    }
}

/// A right-press handler that builds the menu of `build` for `target` at the pointer.
fn context_menu(target: MenuTarget, build: fn(&MenuTarget, &App) -> Vec<MenuItem>) -> MenuFn {
    Box::new(move |ev, window, cx| {
        let items = build(&target, cx);
        menus::open_context_menu(window, cx, ev.position, items);
    })
}
