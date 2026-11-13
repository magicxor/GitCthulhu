//! "Is the app idle?" for the scenario runner: operations finished, loads settled.
//!
//! The runner polls [`busy_reason`]; the app counts as idle when no probe reports work and that stays true for a
//! quiet period, after which the runner also waits for frames to be drawn. Regions whose own asynchronous work is
//! invisible here (for example a diff or a commit-details load) add a probe with [`register_idle_probe`].

use gpui_kit::component::WindowExt as _;
use gpui_kit::{App, Global, Window};

use crate::app_view::{AppViewGlobal, TabContent};
use crate::tab::model::{Loadable, RepoModel};

/// A probe: `Some(reason)` while the part of the app it watches is still working.
pub type IdleProbe = fn(&mut Window, &App) -> Option<String>;

/// The registered probes (an app global).
#[derive(Default)]
pub struct IdleProbes {
    probes: Vec<IdleProbe>,
}

impl Global for IdleProbes {}

/// Adds an idle probe (see the module documentation).
pub fn register_idle_probe(cx: &mut App, probe: IdleProbe) {
    cx.default_global::<IdleProbes>().probes.push(probe);
}

/// Why the app is busy right now, or `None` when it is idle.
pub fn busy_reason(window: &mut Window, cx: &mut App) -> Option<String> {
    // An operation that waits for a dialog (a credential prompt) waits for the scenario, not for the app.
    let dialog_open = window.has_active_dialog(cx);
    if let Some(reason) = app_probe(cx, dialog_open) {
        return Some(reason);
    }
    let probes: Vec<IdleProbe> = cx
        .try_global::<IdleProbes>()
        .map(|p| p.probes.clone())
        .unwrap_or_default();
    probes.into_iter().find_map(|probe| probe(window, cx))
}

/// The built-in probe: repositories being opened, the active tab's operation and loads.
fn app_probe(cx: &App, dialog_open: bool) -> Option<String> {
    let view = cx.try_global::<AppViewGlobal>()?.0.upgrade()?;
    let view = view.read(cx);
    for slot in view.tabs() {
        if let TabContent::Opening { path, .. } = &slot.content {
            return Some(format!("opening {}", path.display()));
        }
    }
    let tab = view.active_tab()?.read(cx);
    if let Some(busy) = tab.busy()
        && !dialog_open
    {
        return Some(format!("operation {:?} running", busy.kind));
    }
    let model = tab.ctx().model.read(cx);
    model_busy(model).or_else(|| views_busy(tab.views(), cx))
}

/// The regions' own asynchronous loads (diff, commit details, blame, file history), so `shot` never catches a
/// half-loaded view.
fn views_busy(views: &crate::tab::RegionViews, cx: &App) -> Option<String> {
    if views.diff.read(cx).is_loading() {
        return Some("the diff is loading".into());
    }
    if views.commit_changes.read(cx).is_loading() {
        return Some("the commit details are loading".into());
    }
    if views
        .blame
        .as_ref()
        .is_some_and(|b| b.read(cx).is_loading())
    {
        return Some("the blame is loading".into());
    }
    if views
        .history
        .as_ref()
        .is_some_and(|(_, h)| h.read(cx).is_loading(cx))
    {
        return Some("the file history is loading".into());
    }
    None
}

/// A load that runs, or that has never finished (no value and no error yet).
fn model_busy(model: &RepoModel) -> Option<String> {
    fn pending<T>(name: &str, load: &Loadable<T>) -> Option<String> {
        (load.loading || (load.value.is_none() && load.error.is_none()))
            .then(|| format!("{name} not loaded"))
    }
    pending("status", &model.status)
        .or_else(|| pending("repository state", &model.state))
        .or_else(|| pending("refs", &model.refs))
        .or_else(|| pending("log", &model.log))
        .or_else(|| (model.submodules.loading).then(|| "submodules loading".to_owned()))
        .or_else(|| (model.worktrees.loading).then(|| "worktrees loading".to_owned()))
        .or_else(|| (model.identity.loading).then(|| "identity loading".to_owned()))
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use std::sync::Arc;

    use super::*;

    #[test]
    fn a_model_without_loaded_data_is_busy_until_everything_arrived() {
        let seed = crate::test_support::RepoSeed::empty();
        let mut model = RepoModel::default();
        assert!(model_busy(&model).is_some(), "nothing loaded yet");
        model.status = Loadable::ready(Arc::new(seed.status.clone()));
        assert!(
            model_busy(&model)
                .expect("still busy")
                .contains("repository state"),
            "the next missing load is named"
        );
        model.state = Loadable::ready(Arc::new(seed.state));
        model.refs.error = Some("boom".into());
        model.log.loading = true;
        assert!(
            model_busy(&model).expect("log loading").contains("log"),
            "a running load is busy"
        );
        model.log = Loadable::ready(Arc::new(crate::tab::model::LogSnapshot {
            rows: Default::default(),
            max_lane: 0,
            exhausted: true,
            generation: 0,
        }));
        assert_eq!(
            model_busy(&model),
            None,
            "a failed load counts as settled (its error is reported elsewhere)"
        );
    }
}
