//! Submodule operations of the catalogue (spec 21 §2.11; owned by G3 after G0b). Final signatures; "Not
//! implemented yet" until G3 implements them.

use gpui_kit::{Context, Window};

use crate::backend::RelaPath;
use crate::tab::RepoTab;

impl RepoTab {
    /// Adds a submodule (`submodule::add`). Owner: G3.
    pub fn add_submodule(
        &mut self,
        _url: String,
        _path: RelaPath,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.not_implemented("Add submodule", cx);
        false
    }

    /// Initialises and updates a submodule (`submodule::init_update`). Owner: G3.
    pub fn init_submodule(
        &mut self,
        _path: RelaPath,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.not_implemented("Init submodule", cx);
        false
    }

    /// Updates a submodule (`submodule::update`). Owner: G3.
    pub fn update_submodule(
        &mut self,
        _path: RelaPath,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.not_implemented("Update submodule", cx);
        false
    }

    /// Synchronises a submodule's URL (`submodule::sync`). Owner: G3.
    pub fn sync_submodule(
        &mut self,
        _path: RelaPath,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.not_implemented("Sync submodule", cx);
        false
    }

    /// Deinitialises a submodule (`submodule::deinit`). Owner: G3.
    pub fn deinit_submodule(
        &mut self,
        _path: RelaPath,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.not_implemented("Deinit submodule", cx);
        false
    }

    /// Removes a submodule (`submodule::remove`). Owner: G3.
    pub fn delete_submodule(
        &mut self,
        _path: RelaPath,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.not_implemented("Delete submodule", cx);
        false
    }
}
