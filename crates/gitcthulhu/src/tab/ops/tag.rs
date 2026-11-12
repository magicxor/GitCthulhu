//! Tag operations of the catalogue (spec 21 §2.11; owned by G3 after G0b). Final signatures; "Not implemented yet"
//! until G3 implements them.

use gpui_kit::{Context, Window};

use crate::backend::ObjectId;
use crate::tab::RepoTab;

impl RepoTab {
    /// Creates a tag (`tag::create_tag`; a message makes it annotated). Owner: G3.
    pub fn create_tag(
        &mut self,
        _name: String,
        _target: ObjectId,
        _message: Option<String>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.not_implemented("Create tag", cx);
        false
    }

    /// Deletes a tag (`tag::delete_tag`). Owner: G3.
    pub fn delete_tag(
        &mut self,
        _name: String,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.not_implemented("Delete tag", cx);
        false
    }
}
