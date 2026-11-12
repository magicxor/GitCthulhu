//! Image, binary, submodule, too-large and no-changes views of the diff pane (owned by G7a; 09 §1.3, §1.7-§1.9).

use std::hash::{Hash as _, Hasher as _};
use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Bounds, Context, Image, ImageFormat, InteractiveElement as _,
    IntoElement, ParentElement as _, Pixels, SharedString, StatefulInteractiveElement as _,
    Styled as _, StyledImage as _, TestSupportExt as _, Window, canvas, div, img, px,
};

use super::{DiffView, ids};
use crate::app_view::{AppViewGlobal, show_toast};
use crate::backend::{
    ByteSlice as _, DiffContent, FileDiff, ImageData, ObjectId, SizeUnit, SubmoduleState,
};
use crate::icons::{AppIcon, icon};
use crate::metrics::{diff as m, dp};
use crate::notifications::ToastKind;
use crate::theme::{ActiveTokens as _, TextStyleExt as _};
use crate::widgets::{BoundsCell, PrimaryButton};

/// One decoded image side.
pub struct ImageEntry {
    /// The image for the `img` element.
    pub image: Arc<Image>,
    /// Pixel size read from the file header (`None` for formats without a cheap header read).
    pub size: Option<(u32, u32)>,
    /// The encoded bytes (written to a temporary file when the image is clicked).
    pub bytes: Arc<[u8]>,
    /// File extension for the temporary file.
    pub ext: &'static str,
}

/// The two sides of an image diff.
pub struct Images {
    pub old: Option<ImageEntry>,
    pub new: Option<ImageEntry>,
}

/// The image format and file extension of a MIME type the image element can decode.
fn format_of(mime: &str) -> Option<(ImageFormat, &'static str)> {
    Some(match mime {
        "image/png" => (ImageFormat::Png, "png"),
        "image/jpeg" => (ImageFormat::Jpeg, "jpg"),
        "image/gif" => (ImageFormat::Gif, "gif"),
        "image/webp" => (ImageFormat::Webp, "webp"),
        "image/bmp" => (ImageFormat::Bmp, "bmp"),
        "image/tiff" => (ImageFormat::Tiff, "tiff"),
        "image/x-icon" | "image/vnd.microsoft.icon" => (ImageFormat::Ico, "ico"),
        _ => return None,
    })
}

fn decode_side(data: &ImageData) -> Option<ImageEntry> {
    let (format, ext) = format_of(data.mime)?;
    Some(ImageEntry {
        image: Arc::new(Image::from_bytes(format, data.bytes.to_vec())),
        size: image_size(&data.bytes),
        bytes: data.bytes.clone(),
        ext,
    })
}

/// Prepares the image sides of an image diff (background). `None` for other content, or when no side has a
/// format the image element can show (the binary view is shown then).
pub fn decode_images(diff: &FileDiff) -> Option<Images> {
    let DiffContent::Image { old, new } = &diff.content else {
        return None;
    };
    let images = Images {
        old: old.as_ref().and_then(decode_side),
        new: new.as_ref().and_then(decode_side),
    };
    (images.old.is_some() || images.new.is_some()).then_some(images)
}

fn be16(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(u16::from_be_bytes([
        *b.get(at)?,
        *b.get(at + 1)?,
    ])))
}

fn le16(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from(u16::from_le_bytes([
        *b.get(at)?,
        *b.get(at + 1)?,
    ])))
}

fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn le24(b: &[u8], at: usize) -> Option<u32> {
    Some(
        u32::from(*b.get(at)?) | u32::from(*b.get(at + 1)?) << 8 | u32::from(*b.get(at + 2)?) << 16,
    )
}

/// Width and height in pixels from the header of a PNG, GIF, BMP, JPEG, WebP or ICO file.
pub fn image_size(b: &[u8]) -> Option<(u32, u32)> {
    let size = if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        (be32(b, 16)?, be32(b, 20)?)
    } else if b.starts_with(b"GIF8") {
        (le16(b, 6)?, le16(b, 8)?)
    } else if b.starts_with(b"BM") {
        let w = i32::from_le_bytes(b.get(18..22)?.try_into().ok()?);
        let h = i32::from_le_bytes(b.get(22..26)?.try_into().ok()?);
        (w.unsigned_abs(), h.unsigned_abs())
    } else if b.starts_with(&[0xFF, 0xD8]) {
        jpeg_size(b)?
    } else if b.starts_with(b"RIFF") && b.get(8..12) == Some(b"WEBP") {
        match b.get(12..16)? {
            b"VP8 " => (le16(b, 26)? & 0x3FFF, le16(b, 28)? & 0x3FFF),
            b"VP8L" => {
                let (b1, b2, b3, b4) = (
                    u32::from(*b.get(21)?),
                    u32::from(*b.get(22)?),
                    u32::from(*b.get(23)?),
                    u32::from(*b.get(24)?),
                );
                (
                    1 + (((b2 & 0x3F) << 8) | b1),
                    1 + (((b4 & 0x0F) << 10) | (b3 << 2) | ((b2 & 0xC0) >> 6)),
                )
            }
            b"VP8X" => (1 + le24(b, 24)?, 1 + le24(b, 27)?),
            _ => return None,
        }
    } else if b.starts_with(&[0, 0, 1, 0]) {
        let side = |v: u8| if v == 0 { 256 } else { u32::from(v) };
        (side(*b.get(6)?), side(*b.get(7)?))
    } else {
        return None;
    };
    (size.0 > 0 && size.1 > 0).then_some(size)
}

fn jpeg_size(b: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2;
    while i + 3 < b.len() {
        if b[i] != 0xFF {
            return None;
        }
        let marker = b[i + 1];
        if marker == 0xFF {
            i += 1;
            continue;
        }
        let is_sof = (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC);
        if is_sof {
            return Some((be16(b, i + 7)?, be16(b, i + 5)?));
        }
        if marker == 0xD8 || (0xD0..=0xD7).contains(&marker) || marker == 0x01 {
            i += 2;
            continue;
        }
        i += 2 + be16(b, i + 2)? as usize;
    }
    None
}

/// The displayed size of an image of `image` pixels in a box of `avail`: scaled down to fit, never up (21 §6
/// deviation 22). One image pixel per logical pixel otherwise (Gitnuro: 1 px per dp).
pub fn fit(image: (f32, f32), avail: (f32, f32)) -> (f32, f32) {
    let (w, h) = image;
    if w <= 0. || h <= 0. {
        return (0., 0.);
    }
    let scale = (avail.0.max(0.) / w).min(avail.1.max(0.) / h).min(1.);
    (w * scale, h * scale)
}

/// An invisible element that records the bounds of its `relative()` parent and redraws once when they change
/// (the image size depends on the box it sits in).
fn measure(cell: &BoundsCell) -> impl IntoElement {
    let cell = cell.clone();
    canvas(
        move |bounds: Bounds<Pixels>, window: &mut Window, _: &mut App| {
            if cell.get() != bounds {
                cell.set(bounds);
                window.request_animation_frame();
            }
        },
        |_, _, _, _| {},
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

/// "No content changes to display" (09 §1.3): a 96 dp info icon and a body1 line, centred.
pub(super) fn no_changes(text: impl Into<SharedString>, cx: &App) -> AnyElement {
    message_view(ids::NO_CHANGES, AppIcon::Info, text.into(), cx)
}

/// A load error: the same layout as "No content changes" with a warning icon.
pub(super) fn error_view(text: SharedString, cx: &App) -> AnyElement {
    message_view(ids::ERROR, AppIcon::Warning, text, cx)
}

fn message_view(id: &'static str, role: AppIcon, text: SharedString, cx: &App) -> AnyElement {
    let tokens = *cx.tokens();
    div()
        .id(id)
        .flex_1()
        .min_h_0()
        .w_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(dp(m::NO_CHANGES_GAP))
        .child(icon(role, m::NO_CHANGES_ICON, tokens.on_background))
        .child(
            div()
                .text_body1()
                .text_color(tokens.on_background)
                .child(text),
        )
        .test_support()
        .into_any_element()
}

/// The text of a too-large diff.
pub fn too_large_text(threshold: u64, unit: SizeUnit) -> String {
    let unit = match unit {
        SizeUnit::Bytes => "bytes",
        SizeUnit::Lines => "lines",
    };
    format!("File too large to display (over {threshold} {unit})")
}

/// Non-text content.
pub(super) fn render_other(
    view: &DiffView,
    diff: &Arc<FileDiff>,
    cx: &mut Context<DiffView>,
) -> AnyElement {
    match &diff.content {
        DiffContent::Binary => binary(cx),
        DiffContent::TooLarge { threshold, unit } => {
            no_changes(too_large_text(*threshold, *unit), cx)
        }
        DiffContent::Image { .. } => match view.loaded.as_ref().and_then(|l| l.images.clone()) {
            Some(images) => images_view(view, &images, cx),
            None => binary(cx),
        },
        DiffContent::Submodule {
            old_commit,
            new_commit,
            ..
        } => submodule(view, diff, *old_commit, *new_commit, cx),
        _ => no_changes("No content changes to display", cx),
    }
}

/// Binary file: the binary icon, 400 dp, tinted `primary` (09 §1.7).
fn binary(cx: &App) -> AnyElement {
    let tokens = *cx.tokens();
    div()
        .flex_1()
        .min_h_0()
        .w_full()
        .flex()
        .items_center()
        .justify_center()
        .overflow_hidden()
        .child(
            div()
                .id(ids::BINARY)
                .flex_none()
                .child(icon(AppIcon::Binary, m::BINARY_ICON, tokens.primary))
                .test_support(),
        )
        .into_any_element()
}

#[derive(Clone, Copy)]
enum Placement {
    /// Both sides: centred horizontally under the title, at the top.
    Column,
    /// Deleted file: top-start.
    TopStart,
    /// Added file: centred both ways.
    Center,
}

fn image_box(
    id: &'static str,
    entry: &ImageEntry,
    cell: &BoundsCell,
    placement: Placement,
) -> AnyElement {
    let avail = cell.get().size;
    let (w, h) = match entry.size {
        Some((w, h)) => fit(
            (w as f32, h as f32),
            (avail.width.as_f32(), avail.height.as_f32()),
        ),
        None => (avail.width.as_f32(), avail.height.as_f32()),
    };
    let image = entry.image.clone();
    let bytes = entry.bytes.clone();
    let ext = entry.ext;
    div()
        .flex_1()
        .min_h_0()
        .w_full()
        .relative()
        .flex()
        .map(|d| match placement {
            Placement::Column => d.justify_center().items_start(),
            Placement::TopStart => d.justify_start().items_start(),
            Placement::Center => d.justify_center().items_center(),
        })
        .child(measure(cell))
        .child(
            div()
                .id(id)
                .flex_none()
                .w(px(w))
                .h(px(h))
                .cursor_pointer()
                .on_click(move |_, _, cx| open_image(bytes.clone(), ext, cx))
                .child(
                    img(image)
                        .size_full()
                        .object_fit(gpui_kit::ObjectFit::ScaleDown),
                )
                .test_support(),
        )
        .into_any_element()
}

/// Image diff (09 §1.7, scaled down to fit: deviation 22).
fn images_view(view: &DiffView, images: &Images, cx: &mut Context<DiffView>) -> AnyElement {
    let tokens = *cx.tokens();
    let title = |text: &'static str| {
        div()
            .flex_none()
            .text_h3()
            .text_color(tokens.on_background)
            .child(text)
    };
    let [old_cell, new_cell] = &view.image_cells;
    match (&images.old, &images.new) {
        (Some(old), Some(new)) => div()
            .flex_1()
            .min_h_0()
            .w_full()
            .flex()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .pl(dp(m::IMAGE_PAD_OUTER))
                    .pr(dp(m::IMAGE_PAD_INNER))
                    .py(dp(m::IMAGE_PAD_OUTER))
                    .child(title("Old"))
                    .child(image_box(ids::IMAGE_OLD, old, old_cell, Placement::Column)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .items_center()
                    .pl(dp(m::IMAGE_PAD_INNER))
                    .pr(dp(m::IMAGE_PAD_OUTER))
                    .py(dp(m::IMAGE_PAD_OUTER))
                    .child(title("New"))
                    .child(image_box(ids::IMAGE_NEW, new, new_cell, Placement::Column)),
            )
            .into_any_element(),
        (Some(old), None) => div()
            .flex_1()
            .min_h_0()
            .w_full()
            .flex()
            .flex_col()
            .p(dp(m::IMAGE_PAD_OUTER))
            .child(image_box(
                ids::IMAGE_OLD,
                old,
                old_cell,
                Placement::TopStart,
            ))
            .into_any_element(),
        (None, Some(new)) => div()
            .flex_1()
            .min_h_0()
            .w_full()
            .flex()
            .flex_col()
            .p(dp(m::IMAGE_PAD_OUTER))
            .child(div().flex_none().h(dp(m::IMAGE_SINGLE_SPACER)))
            .child(image_box(ids::IMAGE_NEW, new, new_cell, Placement::Center))
            .into_any_element(),
        (None, None) => binary(cx),
    }
}

/// Writes the image to a temporary file and opens it with the system's default application (09 §1.7).
fn open_image(bytes: Arc<[u8]>, ext: &'static str, cx: &mut App) {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let write = cx.background_spawn(async move {
        bytes.hash(&mut hasher);
        let dir = crate::paths::cache_dir().join("diff-images");
        let path: PathBuf = dir.join(format!("{:016x}.{ext}", hasher.finish()));
        std::fs::create_dir_all(&dir)?;
        if !path.exists() {
            std::fs::write(&path, &bytes)?;
        }
        Ok::<PathBuf, std::io::Error>(path)
    });
    cx.spawn(async move |cx| {
        let result = write.await;
        cx.update(|cx| match result {
            Ok(path) => super::open_with_system(&path, cx),
            Err(err) => show_toast(
                cx,
                None,
                ToastKind::Error,
                format!("Cannot open the image: {err}"),
                None,
            ),
        });
    })
    .detach();
}

/// Whether "Open submodule" applies (09 §1.8): the submodule is checked out and its index commit is the new one.
fn can_open_submodule(view: &DiffView, diff: &FileDiff, new: Option<ObjectId>, cx: &App) -> bool {
    let model = view.ctx.model.read(cx);
    model.submodules.value.as_ref().is_some_and(|subs| {
        subs.iter().any(|s| {
            s.path == diff.path
                && s.state == SubmoduleState::CheckedOut
                && s.index_commit.is_some()
                && s.index_commit == new
        })
    })
}

/// Submodule pointer change (09 §1.8).
fn submodule(
    view: &DiffView,
    diff: &Arc<FileDiff>,
    old: Option<ObjectId>,
    new: Option<ObjectId>,
    cx: &mut Context<DiffView>,
) -> AnyElement {
    let tokens = *cx.tokens();
    let id_line = |label: &'static str, id: Option<ObjectId>| {
        div()
            .flex()
            .child(div().semibold().child(label))
            .child(SharedString::from(
                id.map_or_else(|| "none".to_owned(), |id| id.to_string()),
            ))
    };
    let open = can_open_submodule(view, diff, new, cx).then(|| {
        let path = diff.path.clone();
        let workdir = view.ctx.backend.handle().and_then(|h| h.workdir());
        div().pt(dp(m::SUBMODULE_BUTTON_PAD_TOP)).child(
            PrimaryButton::new(ids::OPEN_SUBMODULE, "Open submodule").on_click(
                move |window, cx| {
                    let Some(dir) = workdir.as_ref() else {
                        return;
                    };
                    let target = dir.join(path.to_str_lossy().as_ref());
                    if let Some(app) = cx.try_global::<AppViewGlobal>().and_then(|g| g.0.upgrade())
                    {
                        app.update(cx, |app, cx| app.open_repository(target, true, window, cx));
                    }
                },
            ),
        )
    });
    div()
        .id(ids::SUBMODULE)
        .flex_1()
        .min_h_0()
        .w_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .text_color(tokens.on_background)
        .child(
            div()
                .text_h4()
                .semibold()
                .pb(dp(m::SUBMODULE_TITLE_PAD_BOTTOM))
                .child(SharedString::from(format!(
                    "Submodule {}",
                    diff.path.to_str_lossy()
                ))),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .text_body1()
                .child(id_line("Old ID: ", old))
                .child(id_line("New ID: ", new)),
        )
        .children(open)
        .test_support()
        .into_any_element()
}
