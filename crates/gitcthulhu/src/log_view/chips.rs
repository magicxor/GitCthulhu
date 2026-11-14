//! Ref chips of the log rows (owned by G4 after G0b; 08 §2.12); local-branch chips end with
//! `forge::badges::chip_segment(..)` (D0.1).
//!
//! The chips of every commit are indexed once per refs load ([`ChipIndex`], one `HashMap` lookup per visible row).
//! Order on a row: tags (tag-list order), then local branches (alphabetical), then remote branches (remote by remote,
//! alphabetical). With "group branches with remotes" a local branch absorbs its upstream when both point to the same
//! commit and share the short name: the remote is shown as a cloud segment at the end of the local chip.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::{
    AnyElement, AnyView, AppContext as _, ClickEvent, Context, Hsla, InteractiveElement as _,
    IntoElement, MouseButton, MouseDownEvent, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, div,
};
use smallvec::SmallVec;

use super::{LogView, ids};
use crate::backend::{ByteSlice as _, FullName, HeadState, ObjectId, ObjectKind, RefList};
use crate::icons::{AppIcon, icon};
use crate::menus::{self, MenuKind, MenuTarget};
use crate::metrics::dp;
use crate::metrics::log as m;
use crate::theme::{TextStyleExt as _, Tokens};
use crate::widgets::InstantTooltip;

/// An end of a chip (08 §2.12).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChipEnd {
    Left,
    Right,
}

/// Rounds the outer corners of the chip segment at `end` with the chip radius. The chip's own `overflow_hidden`
/// clips to a rectangle only, so every segment with a background rounds its outer end itself.
pub fn round_chip_end<E: gpui_kit::Styled>(segment: E, end: ChipEnd) -> E {
    let radius = dp(m::CHIP_RADIUS);
    match end {
        ChipEnd::Left => segment.rounded_l(radius),
        ChipEnd::Right => segment.rounded_r(radius),
    }
}

/// The left cap of a chip: the icon box.
pub(super) fn icon_box(role: AppIcon, bg: Hsla, icon_color: Hsla) -> gpui_kit::Div {
    round_chip_end(
        div()
            .flex_none()
            .size(dp(m::CHIP_H))
            .flex()
            .items_center()
            .justify_center()
            .bg(bg),
        ChipEnd::Left,
    )
    .child(icon(role, m::CHIP_ICON, icon_color))
}

/// The tracking-remote cell; the right cap of the chip unless a PR segment follows.
pub(super) fn cloud_cell(last: bool, colors: ChipColors) -> gpui_kit::Div {
    let cell = div()
        .flex_none()
        .size(dp(m::CHIP_H))
        .flex()
        .items_center()
        .justify_center()
        .bg(colors.remote)
        .child(icon(AppIcon::Cloud, m::CHIP_ICON, colors.tokens.background));
    if last {
        round_chip_end(cell, ChipEnd::Right)
    } else {
        cell
    }
}

/// What a chip stands for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ChipKind {
    Tag,
    LocalBranch,
    RemoteBranch,
    /// `HEAD` on a detached commit.
    DetachedHead,
}

/// The upstream merged into a local-branch chip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrackingSegment {
    /// `refs/remotes/<remote>/<name>`.
    pub full: FullName,
    /// `<remote>/<name>`.
    pub name: SharedString,
}

/// One chip of a row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chip {
    pub kind: ChipKind,
    /// The text: tag name, branch name without `refs/heads/`, `origin/main`, `HEAD`.
    pub name: SharedString,
    /// The ref (`refs/tags/..`, `refs/heads/..`, `refs/remotes/..`); `None` for a detached HEAD.
    pub full: Option<FullName>,
    /// The commit the ref points to.
    pub target: ObjectId,
    /// The current branch (or the detached HEAD): shows the indicator dot.
    pub is_current: bool,
    /// The upstream of a local branch, when it is grouped into this chip.
    pub tracking: Option<TrackingSegment>,
    /// The local branch's configured upstream (menus).
    pub upstream: Option<FullName>,
    /// Another worktree has the local branch checked out.
    pub checked_out_elsewhere: bool,
}

/// The chips of every decorated commit of one refs load.
#[derive(Default)]
pub struct ChipIndex {
    source: Option<Arc<RefList>>,
    grouped: bool,
    map: HashMap<ObjectId, SmallVec<[Rc<Chip>; 2]>>,
}

impl ChipIndex {
    /// Rebuilds the index when `refs` or the grouping setting changed since the last call.
    pub fn sync(&mut self, refs: Option<&Arc<RefList>>, grouped: bool) {
        let same = match (&self.source, refs) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        if same && self.grouped == grouped {
            return;
        }
        self.map = refs.map(|r| build(r, grouped)).unwrap_or_default();
        self.source = refs.cloned();
        self.grouped = grouped;
    }

    /// The chips of commit `id`, in display order.
    pub fn get(&self, id: &ObjectId) -> &[Rc<Chip>] {
        self.map.get(id).map(|c| c.as_slice()).unwrap_or(&[])
    }
}

fn short(full: &FullName) -> String {
    full.shorten().to_str_lossy().into_owned()
}

/// The chips of every commit of `refs` (08 §2.12).
pub fn build(refs: &RefList, grouped: bool) -> HashMap<ObjectId, SmallVec<[Rc<Chip>; 2]>> {
    let mut map: HashMap<ObjectId, SmallVec<[Rc<Chip>; 2]>> = HashMap::new();
    for tag in refs
        .tags
        .iter()
        .filter(|t| t.peeled_kind == ObjectKind::Commit)
    {
        map.entry(tag.target).or_default().push(Rc::new(Chip {
            kind: ChipKind::Tag,
            name: tag.name.clone().into(),
            full: Some(tag.full.clone()),
            target: tag.target,
            is_current: false,
            tracking: None,
            upstream: None,
            checked_out_elsewhere: false,
        }));
    }
    if let HeadState::Detached { target } = &refs.head {
        map.entry(*target).or_default().push(Rc::new(Chip {
            kind: ChipKind::DetachedHead,
            name: "HEAD".into(),
            full: None,
            target: *target,
            is_current: true,
            tracking: None,
            upstream: None,
            checked_out_elsewhere: false,
        }));
    }
    let mut absorbed: Vec<&FullName> = Vec::new();
    for branch in &refs.local {
        let upstream = branch.upstream.as_ref().and_then(|u| u.tracking.clone());
        let tracking = grouped
            .then(|| {
                let tracking = upstream.as_ref()?;
                refs.remotes.iter().flat_map(|r| &r.branches).find(|rb| {
                    &rb.full == tracking && rb.name == branch.name && rb.target == branch.target
                })
            })
            .flatten();
        if let Some(rb) = tracking {
            absorbed.push(&rb.full);
        }
        map.entry(branch.target).or_default().push(Rc::new(Chip {
            kind: ChipKind::LocalBranch,
            name: branch.name.clone().into(),
            full: Some(branch.full.clone()),
            target: branch.target,
            is_current: branch.is_head,
            tracking: tracking.map(|rb| TrackingSegment {
                full: rb.full.clone(),
                name: format!("{}/{}", rb.remote, rb.name).into(),
            }),
            upstream,
            checked_out_elsewhere: branch.checked_out_in.is_some(),
        }));
    }
    for remote in &refs.remotes {
        for rb in &remote.branches {
            // `origin/HEAD` is a pointer to the remote's default branch, not a branch (08 §2.12).
            if rb.name == "HEAD" || absorbed.contains(&&rb.full) {
                continue;
            }
            map.entry(rb.target).or_default().push(Rc::new(Chip {
                kind: ChipKind::RemoteBranch,
                name: short(&rb.full).into(),
                full: Some(rb.full.clone()),
                target: rb.target,
                is_current: false,
                tracking: None,
                upstream: None,
                checked_out_elsewhere: false,
            }));
        }
    }
    map
}

/// The tooltip of a tracking cloud (08 §2.12; plain text: `InstantTooltip` has no SemiBold runs).
pub fn tracking_tooltip(local: &str, upstream: &str) -> String {
    format!("Local branch {local} is up to date with remote branch {upstream}")
}

/// The colours of a row's chips.
#[derive(Clone, Copy)]
pub struct ChipColors {
    /// The commit's lane colour (border of every chip, icon box of tags and local branches).
    pub lane: Hsla,
    /// Icon box of remote branches (#BD9B46).
    pub remote: Hsla,
    pub tokens: Tokens,
}

impl LogView {
    /// The chip elements of layout row `row` (08 §2.12).
    pub(super) fn render_chips(
        &self,
        row: usize,
        chips: &[Rc<Chip>],
        colors: ChipColors,
        cx: &mut Context<Self>,
    ) -> SmallVec<[AnyElement; 4]> {
        chips
            .iter()
            .enumerate()
            .map(|(k, chip)| self.render_chip(row, k, chip, colors, cx))
            .collect()
    }

    fn render_chip(
        &self,
        row: usize,
        k: usize,
        chip: &Rc<Chip>,
        colors: ChipColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = colors.tokens;
        let (role, icon_bg) = match chip.kind {
            ChipKind::Tag => (AppIcon::Tag, colors.lane),
            ChipKind::LocalBranch | ChipKind::DetachedHead => (AppIcon::Branch, colors.lane),
            ChipKind::RemoteBranch => (AppIcon::Cloud, colors.remote),
        };
        let icon_box = icon_box(role, icon_bg, t.background);
        let current = chip.is_current.then(|| {
            div()
                .flex_none()
                .size(dp(m::CHIP_CURRENT))
                .mr(dp(m::CHIP_CURRENT_PAD_END))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .size(dp(m::CHIP_CURRENT_DOT))
                        .rounded_full()
                        .bg(t.primary_variant),
                )
        });
        let badge = match (&chip.kind, &chip.full) {
            (ChipKind::LocalBranch, Some(full)) => {
                crate::forge::badges::chip_segment(&self.ctx, full, cx)
            }
            _ => None,
        };
        let cloud = chip.tracking.as_ref().map(|tracking| {
            let cell = cloud_cell(badge.is_none(), colors);
            self.render_cloud(row, k, chip, tracking, cell, cx)
        });
        let id = ids::chip(row, k);
        let menu_chip = chip.clone();
        let click_chip = chip.clone();
        div()
            .flex_none()
            .px(dp(m::CHIP_MARGIN_H))
            .child(
                div()
                    .id(id)
                    .relative()
                    .flex_none()
                    .h(dp(m::CHIP_H))
                    .flex()
                    .flex_row()
                    .items_center()
                    .rounded(dp(m::CHIP_RADIUS))
                    .overflow_hidden()
                    .cursor_pointer()
                    .child(icon_box)
                    .child(
                        div()
                            .flex_none()
                            .px(dp(m::CHIP_TEXT_PAD_H))
                            .text_body2()
                            .text_color(t.on_background)
                            .whitespace_nowrap()
                            .child(chip.name.clone()),
                    )
                    .children(current)
                    .children(cloud)
                    .children(badge)
                    // The 2 dp border is drawn over the content edge (08 §2.12).
                    .child(
                        div()
                            .absolute()
                            .inset_0()
                            .rounded(dp(m::CHIP_RADIUS))
                            .border(dp(m::CHIP_BORDER))
                            .border_color(colors.lane),
                    )
                    .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
                        // A click on a chip selects its row like a click on the row itself; a double click also
                        // checks the ref out. The chip handles the click, so the row's listener does not run again.
                        cx.stop_propagation();
                        this.select_commit(click_chip.target, false, row, cx);
                        window.focus(&this.focus, cx);
                        if ev.click_count() >= 2 {
                            this.checkout_chip(&click_chip, window, cx);
                        }
                    }))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            this.open_chip_menu(&menu_chip, None, ev, window, cx);
                        }),
                    )
                    .test_support(),
            )
            .into_any_element()
    }

    /// The tracking-remote segment at the end of a grouped local-branch chip, with its own menu and tooltip.
    fn render_cloud(
        &self,
        row: usize,
        k: usize,
        chip: &Chip,
        tracking: &TrackingSegment,
        cell: gpui_kit::Div,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // The tooltip text is built only when it shows (21 §4.2: no per-frame strings).
        let (local, upstream) = (chip.name.clone(), tracking.name.clone());
        let menu_chip = chip.clone();
        let remote = tracking.clone();
        cell.id(ids::chip_cloud(row, k))
            .tooltip(move |_, cx| {
                let tip = tracking_tooltip(&local, &upstream);
                AnyView::from(cx.new(|_| InstantTooltip::new(tip, None)))
            })
            .tooltip_show_delay(std::time::Duration::ZERO)
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                    cx.stop_propagation();
                    this.open_chip_menu(&menu_chip, Some(&remote), ev, window, cx);
                }),
            )
            .test_support()
            .into_any_element()
    }

    /// Double-click on a chip: check out the ref (08 §2.12).
    fn checkout_chip(&self, chip: &Chip, window: &mut gpui_kit::Window, cx: &mut gpui_kit::App) {
        let Some(full) = chip.full.clone() else {
            return;
        };
        let target = chip.target;
        match chip.kind {
            ChipKind::Tag => self.ctx.with_tab(window, cx, move |tab, window, cx| {
                tab.checkout_tag(full, target, window, cx);
            }),
            ChipKind::LocalBranch => {
                if chip.is_current {
                    return;
                }
                self.ctx.with_tab(window, cx, move |tab, window, cx| {
                    tab.checkout_branch(full, window, cx);
                })
            }
            ChipKind::RemoteBranch => self.ctx.with_tab(window, cx, move |tab, window, cx| {
                tab.checkout_remote_branch(full, None, window, cx);
            }),
            ChipKind::DetachedHead => {}
        }
    }

    /// The menu of a chip (`remote` = its tracking segment): the branch, remote-branch or tag menu of G3.
    fn open_chip_menu(
        &self,
        chip: &Chip,
        remote: Option<&TrackingSegment>,
        ev: &MouseDownEvent,
        window: &mut gpui_kit::Window,
        cx: &mut gpui_kit::App,
    ) {
        let mut target = MenuTarget {
            kind: MenuKind::LocalBranch,
            tab: self.ctx.clone(),
            branch: chip.full.clone(),
            upstream: chip.upstream.clone(),
            commit: Some(chip.target),
            path: None,
            is_current: chip.is_current,
            checked_out_elsewhere: chip.checked_out_elsewhere,
        };
        let items = match (remote, &chip.kind) {
            // The tracking cloud uses the remote branch's own state (fix of 08 §9.8).
            (Some(remote), _) => {
                target.kind = MenuKind::RemoteBranch;
                target.branch = Some(remote.full.clone());
                target.upstream = None;
                target.is_current = false;
                target.checked_out_elsewhere = false;
                menus::refs::remote_branch(&target, cx)
            }
            (None, ChipKind::Tag) => {
                target.kind = MenuKind::Tag;
                menus::refs::tag(&target, cx)
            }
            (None, ChipKind::RemoteBranch) => {
                target.kind = MenuKind::RemoteBranch;
                menus::refs::remote_branch(&target, cx)
            }
            (None, ChipKind::LocalBranch) => menus::refs::local_branch(&target, cx),
            (None, ChipKind::DetachedHead) => Vec::new(),
        };
        menus::open_context_menu(window, cx, ev.position, items);
    }
}
