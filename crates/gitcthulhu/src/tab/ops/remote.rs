//! Remote operations of the catalogue (spec 21 §2.11; owned by G3 after G0b). Final signatures; "Not implemented
//! yet" until G3 implements them.

use gpui_kit::{Context, Window};

use crate::tab::RepoTab;

impl RepoTab {
    /// Adds a remote (`remote::add_remote`). Owner: G3.
    pub fn add_remote(
        &mut self,
        _name: String,
        _url: String,
        _push_url: Option<String>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.not_implemented("Add remote", cx);
        false
    }

    /// Changes a remote's URLs (`remote::set_remote_urls`). Owner: G3.
    pub fn edit_remote(
        &mut self,
        _name: String,
        _url: String,
        _push_url: Option<String>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.not_implemented("Edit remote", cx);
        false
    }

    /// Removes a remote (`remote::remove_remote`). Owner: G3.
    pub fn delete_remote(
        &mut self,
        _name: String,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.not_implemented("Delete remote", cx);
        false
    }
}
