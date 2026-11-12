//! The About dialog (G10; 09 §3.14): a 600 x 600 scrolling list with the application description, the status of
//! the helper programs (the credential helper and the editor helper) and the open source projects it builds on,
//! and a Close link.

use gpui_kit::component::WindowExt as _;
use gpui_kit::{
    AnyElement, App, ElementId, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div,
};

use super::register::common;
use super::{DialogFrame, DialogSpec};
use crate::backend::validate;
use crate::metrics::dialogs::{ABOUT_H, ABOUT_W};
use crate::metrics::{self, dp};
use crate::theme::{ActiveTokens as _, TextStyleExt as _, Tokens};
use crate::widgets::scroll::scroll_y;
use crate::widgets::tooltip::TooltipExt as _;
use crate::widgets::{TextLink, observed};

/// Element ids of the About dialog.
pub mod ids {
    /// The dialog content.
    pub const ABOUT_DIALOG: &str = "about-dialog";
    /// The status row of the credential helper.
    pub const ABOUT_HELPER_ASKPASS: &str = "about-helper-askpass";
    /// The status row of the editor helper.
    pub const ABOUT_HELPER_EDITOR: &str = "about-helper-editor";
    /// The Close link.
    pub const ABOUT_CLOSE: &str = "about-close";
    /// Prefix of the project links: `(ABOUT_PROJECT, index)`; licences are `(ABOUT_LICENSE, index)`.
    pub const ABOUT_PROJECT: &str = "about-project";
    /// Prefix of the licence links.
    pub const ABOUT_LICENSE: &str = "about-license";
}

/// What GitCthulhu is.
pub const DESCRIPTION: &str = "GitCthulhu is a plain, fast Git client: it manages multiple repositories with a familiar interface, a live view of the commit graph and your working tree, and undo for what it does.";

/// A project the application builds on: name, address, licence name, licence address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Project {
    /// Display name.
    pub name: &'static str,
    /// Home page.
    pub url: &'static str,
    /// Licence name.
    pub license: &'static str,
    /// Licence text address.
    pub license_url: &'static str,
}

/// The listed projects (rows of the dialog).
pub const PROJECTS: [Project; 10] = [
    Project {
        name: "Git",
        url: "https://git-scm.com",
        license: "GPL-2.0",
        license_url: "https://git-scm.com/about/free-and-open-source",
    },
    Project {
        name: "GPUI",
        url: "https://www.gpui.rs",
        license: "Apache 2.0",
        license_url: "https://www.apache.org/licenses/LICENSE-2.0",
    },
    Project {
        name: "gpui-kit",
        url: "https://github.com/longbridge/gpui-kit",
        license: "Apache 2.0",
        license_url: "https://www.apache.org/licenses/LICENSE-2.0",
    },
    Project {
        name: "gitoxide",
        url: "https://github.com/GitoxideLabs/gitoxide",
        license: "MIT / Apache 2.0",
        license_url: "https://github.com/GitoxideLabs/gitoxide/blob/main/LICENSE-MIT",
    },
    Project {
        name: "GitButler",
        url: "https://github.com/gitbutlerapp/gitbutler",
        license: "FSL-1.1-MIT",
        license_url: "https://fsl.software",
    },
    Project {
        name: "Gitnuro (layout reference)",
        url: "https://github.com/JetpackDuba/Gitnuro",
        license: "GPL-3.0",
        license_url: "https://www.gnu.org/licenses/gpl-3.0.html",
    },
    Project {
        name: "Lucide icons",
        url: "https://lucide.dev",
        license: "ISC",
        license_url: "https://lucide.dev/license",
    },
    Project {
        name: "Inter",
        url: "https://rsms.me/inter",
        license: "OFL-1.1",
        license_url: "https://openfontlicense.org",
    },
    Project {
        name: "Geist Mono",
        url: "https://vercel.com/font",
        license: "OFL-1.1",
        license_url: "https://openfontlicense.org",
    },
    Project {
        name: "syntect",
        url: "https://github.com/trishume/syntect",
        license: "MIT",
        license_url: "https://github.com/trishume/syntect/blob/master/LICENSE-MIT",
    },
];

/// One line of the helper status: the text and whether the helper is usable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HelperLine {
    /// "Credential helper (gitcthulhu-askpass)".
    pub label: &'static str,
    /// The path, or "missing: <path>".
    pub text: SharedString,
    /// The program exists.
    pub ok: bool,
}

/// The helper status lines for the running Git runtime; one "not available" line without a runtime.
pub fn helper_lines(cx: &App) -> Vec<HelperLine> {
    let Some(rt) = crate::app::git_runtime(cx) else {
        return vec![HelperLine {
            label: "Git",
            text: "Git is not available".into(),
            ok: false,
        }];
    };
    let status = validate::helper_status(&rt);
    let line =
        |label: &'static str, path: Result<std::path::PathBuf, std::path::PathBuf>| match path {
            Ok(path) => HelperLine {
                label,
                text: path.display().to_string().into(),
                ok: true,
            },
            Err(path) => HelperLine {
                label,
                text: format!("missing: {}", path.display()).into(),
                ok: false,
            },
        };
    vec![
        line("Credential helper (gitcthulhu-askpass)", status.askpass),
        line("Editor helper (gitcthulhu-editor)", status.editor),
    ]
}

/// Gitnuro's inverted `TextLink` (licence names): `on_background` normally, `primary_variant` on hover.
fn inverted_link(
    id: impl Into<ElementId>,
    text: &'static str,
    url: &'static str,
    tokens: &Tokens,
) -> impl IntoElement {
    let hover = tokens.primary_variant;
    div()
        .id(id)
        .flex_none()
        .whitespace_nowrap()
        .text_color(tokens.on_background)
        .cursor_pointer()
        .hover(move |s| s.text_color(hover))
        .on_click(move |_, _, cx| common::open_url(cx, url))
        .child(text)
        .delayed_tooltip(url)
        .test_support()
}

fn content(helpers: &[HelperLine], cx: &mut App) -> AnyElement {
    let tokens = *cx.tokens();
    let helpers = helpers.to_vec();
    let helper_ids = [ids::ABOUT_HELPER_ASKPASS, ids::ABOUT_HELPER_EDITOR];
    let helper_rows = helpers.into_iter().enumerate().map(|(ix, line)| {
        let row = div()
            .w_full()
            .px(dp(16.))
            .py(dp(4.))
            .flex()
            .flex_col()
            .child(div().text_body2().semibold().child(line.label))
            .child(
                div()
                    .text_body2()
                    .text_color(if line.ok {
                        tokens.on_background_secondary
                    } else {
                        tokens.error
                    })
                    .child(line.text),
            );
        match helper_ids.get(ix) {
            Some(id) => observed(*id).child(row).into_any_element(),
            None => row.into_any_element(),
        }
    });
    let projects = PROJECTS.iter().enumerate().map(|(ix, project)| {
        let (url, license_url) = (project.url, project.license_url);
        div()
            .w_full()
            .px(dp(16.))
            .flex()
            .flex_row()
            .items_center()
            .child(
                div().py(dp(8.)).child(
                    TextLink::new((ids::ABOUT_PROJECT, ix), project.name)
                        .tooltip(url)
                        .on_click(move |_, cx| common::open_url(cx, url)),
                ),
            )
            .child(div().flex_1())
            .child(inverted_link(
                (ids::ABOUT_LICENSE, ix),
                project.license,
                license_url,
                &tokens,
            ))
    });
    observed(ids::ABOUT_DIALOG)
        .w(dp(ABOUT_W))
        .h(dp(ABOUT_H))
        .flex()
        .flex_col()
        .child(
            scroll_y("about-scroll")
                .flex_1()
                .min_h_0()
                .w_full()
                .child(
                    div()
                        .p(dp(16.))
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_baseline()
                                .gap(dp(8.))
                                .child(div().text_h2().child("GitCthulhu"))
                                .child(
                                    div()
                                        .text_caption()
                                        .text_color(tokens.on_background_secondary)
                                        .child(format!("version {}", env!("CARGO_PKG_VERSION"))),
                                ),
                        )
                        .child(div().pt(dp(16.)).text_body1().child(DESCRIPTION))
                        .child(
                            div()
                                .pt(dp(16.))
                                .text_body1()
                                .semibold()
                                .child("Helper programs"),
                        ),
                )
                .children(helper_rows)
                .child(div().p(dp(16.)).text_body1().child(
                    "GitCthulhu has been possible thanks to the following open source projects:",
                ))
                .children(projects),
        )
        .child(
            div()
                .pt(dp(metrics::DIALOG_PAD))
                .pr(dp(8.))
                .flex()
                .flex_row()
                .justify_end()
                .child(
                    TextLink::new(ids::ABOUT_CLOSE, "Close").on_click(|window, cx| {
                        window.close_dialog(cx);
                    }),
                ),
        )
        .into_any_element()
}

/// Opens the About dialog.
pub fn open(window: &mut Window, cx: &mut App) {
    // The helper status checks the file system: read it once, not every time the kit rebuilds the layer.
    let helpers = helper_lines(cx);
    DialogFrame::open(
        window,
        cx,
        DialogSpec {
            width: ABOUT_W + 2. * metrics::DIALOG_PAD,
            est_height: ABOUT_H + 2. * metrics::DIALOG_PAD,
        },
        move |_, cx| content(&helpers, cx),
    );
}
