//! Avatars (Gitnuro 08 §2.9, `AvatarImage`): the author's Gravatar image, or the first letter of the name on a
//! coloured background until (or unless) the image is available.
//!
//! The app owns no HTTP stack here: milestone 2 (X5) installs an [`AvatarFetcher`] with [`set_fetcher`] that
//! downloads `gravatar_url(email)`. Without a fetcher the initials are shown.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::{
    App, AsyncApp, Global, Hsla, Image, ImageFormat, IntoElement, ObjectFit, ParentElement as _,
    RenderOnce, SharedString, Styled as _, StyledImage as _, Task, Window, div, img, white,
};
use sha2::{Digest, Sha256};

use crate::metrics::dp;
use crate::settings::{self, AvatarProvider};
use crate::theme::{ActiveTokens as _, TextStyleExt as _};

/// Downloads the bytes of an image URL. Installed by the services layer.
pub type AvatarFetcher = Rc<dyn Fn(String, &mut App) -> Task<anyhow::Result<Vec<u8>>>>;

/// Where an avatar is in its life cycle.
#[derive(Clone)]
enum AvatarState {
    Loading,
    Ready(Arc<Image>),
    Missing,
}

/// Cache of avatar images keyed by lowercase e-mail address.
#[derive(Default)]
pub struct AvatarCache {
    states: HashMap<String, AvatarState>,
    fetcher: Option<AvatarFetcher>,
    /// Counts [`retry_missing`] calls: a download started before the newest one that fails is not remembered as
    /// missing (it is asked again), so a late answer cannot undo a retry.
    generation: u64,
}

impl Global for AvatarCache {}

/// The Gravatar URL for `email` (SHA-256 of the trimmed, lowercased address; 404 when there is no image).
pub fn gravatar_url(email: &str) -> String {
    let digest = Sha256::digest(email.trim().to_lowercase().as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("https://www.gravatar.com/avatar/{hex}?s=80&d=404")
}

/// Installs the function that downloads avatar images and drops negative cache entries so they are retried.
pub fn set_fetcher(cx: &mut App, fetcher: AvatarFetcher) {
    cx.default_global::<AvatarCache>().fetcher = Some(fetcher);
    retry_missing(cx);
}

/// Forgets the avatars whose download failed, so they are asked for again when next shown (the downloader's retry
/// after being offline). Downloads still running then are asked again too if they fail.
pub fn retry_missing(cx: &mut App) {
    let cache = cx.default_global::<AvatarCache>();
    cache.generation += 1;
    cache
        .states
        .retain(|_, state| !matches!(state, AvatarState::Missing));
    cx.refresh_windows();
}

fn image_format(bytes: &[u8]) -> Option<ImageFormat> {
    match bytes {
        [0x89, b'P', b'N', b'G', ..] => Some(ImageFormat::Png),
        [0xFF, 0xD8, 0xFF, ..] => Some(ImageFormat::Jpeg),
        [b'G', b'I', b'F', b'8', ..] => Some(ImageFormat::Gif),
        [
            b'R',
            b'I',
            b'F',
            b'F',
            _,
            _,
            _,
            _,
            b'W',
            b'E',
            b'B',
            b'P',
            ..,
        ] => Some(ImageFormat::Webp),
        _ => None,
    }
}

/// Starts the download of the avatar of `email` unless it is cached or in flight.
fn request(cx: &mut App, key: String, email: String) {
    let cache = cx.default_global::<AvatarCache>();
    if cache.states.contains_key(&key) {
        return;
    }
    let Some(fetcher) = cache.fetcher.clone() else {
        return;
    };
    cache.states.insert(key.clone(), AvatarState::Loading);
    let generation = cache.generation;
    let download = fetcher(gravatar_url(&email), cx);
    cx.spawn(async move |cx: &mut AsyncApp| {
        let state = match download.await {
            Ok(bytes) => match image_format(&bytes) {
                Some(format) => AvatarState::Ready(Arc::new(Image::from_bytes(format, bytes))),
                None => AvatarState::Missing,
            },
            Err(_) => AvatarState::Missing,
        };
        cx.update(|cx| {
            settle(cx.default_global::<AvatarCache>(), key, generation, state);
            cx.refresh_windows();
        });
    })
    .detach();
}

/// Records the answer of a download started at `generation`: a failure from before the newest retry is forgotten
/// rather than remembered, so the avatar is asked for again.
fn settle(cache: &mut AvatarCache, key: String, generation: u64, state: AvatarState) {
    if matches!(state, AvatarState::Missing) && generation != cache.generation {
        cache.states.remove(&key);
    } else {
        cache.states.insert(key, state);
    }
}

/// The avatar shape: a circle, or a rounded square for merge commits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AvatarShape {
    Circle,
    RoundedSquare,
}

/// An avatar element.
#[derive(IntoElement)]
pub struct Avatar {
    name: SharedString,
    email: SharedString,
    size: f32,
    shape: AvatarShape,
    fallback: Option<Hsla>,
}

impl Avatar {
    /// An avatar of `size_dp` for the author `name` and `email`.
    pub fn new(
        name: impl Into<SharedString>,
        email: impl Into<SharedString>,
        size_dp: f32,
    ) -> Self {
        Self {
            name: name.into(),
            email: email.into(),
            size: size_dp,
            shape: AvatarShape::Circle,
            fallback: None,
        }
    }

    /// Sets the shape.
    pub fn shape(mut self, shape: AvatarShape) -> Self {
        self.shape = shape;
        self
    }

    /// Background of the initials fallback (default `primary`; the log uses the lane colour).
    pub fn fallback_color(mut self, color: Hsla) -> Self {
        self.fallback = Some(color);
        self
    }

    /// The fallback letter: the first character of the name, uppercased, or `#` when there is none.
    pub fn initial(name: &str) -> String {
        name.trim()
            .chars()
            .next()
            .map(|c| c.to_uppercase().collect())
            .unwrap_or_else(|| "#".to_owned())
    }
}

impl RenderOnce for Avatar {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let tokens = *cx.tokens();
        let key = self.email.trim().to_lowercase();
        let wants_image = !key.is_empty()
            && settings::get_or_default(cx).avatar_provider == AvatarProvider::Gravatar;
        let mut image = None;
        if wants_image {
            match cx
                .try_global::<AvatarCache>()
                .and_then(|c| c.states.get(&key))
                .cloned()
            {
                Some(AvatarState::Ready(i)) => image = Some(i),
                Some(AvatarState::Loading | AvatarState::Missing) => {}
                None => request(cx, key, self.email.to_string()),
            }
        }
        let frame = div()
            .flex_none()
            .size(dp(self.size))
            .overflow_hidden()
            .map_shape(self.shape);
        match image {
            Some(image) => frame
                .child(img(image).size_full().object_fit(ObjectFit::Cover))
                .into_any_element(),
            None => frame
                .flex()
                .items_center()
                .justify_center()
                .bg(self.fallback.unwrap_or(tokens.primary))
                .text_body1()
                .text_color(white())
                .child(Self::initial(&self.name))
                .into_any_element(),
        }
    }
}

trait ShapeExt: gpui_kit::Styled + Sized {
    fn map_shape(self, shape: AvatarShape) -> Self {
        match shape {
            AvatarShape::Circle => self.rounded_full(),
            AvatarShape::RoundedSquare => self.rounded(dp(4.)),
        }
    }
}

impl<T: gpui_kit::Styled> ShapeExt for T {}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use super::*;

    #[test]
    fn initial_is_the_uppercased_first_letter_or_hash() {
        assert_eq!(Avatar::initial("ilia"), "I");
        assert_eq!(Avatar::initial("  ärger"), "Ä");
        assert_eq!(Avatar::initial(""), "#");
    }

    #[test]
    fn gravatar_url_hashes_the_normalised_address() {
        assert_eq!(
            gravatar_url("  A@Example.com "),
            gravatar_url("a@example.com"),
            "case and whitespace do not change the hash"
        );
        let url = gravatar_url("a@example.com");
        assert!(url.starts_with("https://www.gravatar.com/avatar/"), "{url}");
        assert!(url.ends_with("?s=80&d=404"), "{url}");
        assert_eq!(
            url.len(),
            "https://www.gravatar.com/avatar/".len() + 64 + "?s=80&d=404".len()
        );
    }

    #[test]
    fn a_failure_from_before_a_retry_is_asked_again() {
        let mut cache = AvatarCache::default();
        let started = cache.generation;
        cache.states.insert("a@x".into(), AvatarState::Loading);
        cache.states.insert("b@x".into(), AvatarState::Loading);
        // The retry fires while both downloads still run.
        cache.generation += 1;
        settle(&mut cache, "a@x".into(), started, AvatarState::Missing);
        assert!(
            !cache.states.contains_key("a@x"),
            "a late failure is forgotten, so the next render asks again"
        );
        let image = Arc::new(Image::from_bytes(ImageFormat::Png, vec![0x89, b'P']));
        settle(&mut cache, "b@x".into(), started, AvatarState::Ready(image));
        assert!(
            matches!(cache.states.get("b@x"), Some(AvatarState::Ready(_))),
            "a late image is still used"
        );
        let now = cache.generation;
        settle(&mut cache, "c@x".into(), now, AvatarState::Missing);
        assert!(
            matches!(cache.states.get("c@x"), Some(AvatarState::Missing)),
            "a failure since the retry is remembered until the next one"
        );
    }

    #[test]
    fn image_formats_are_detected_by_magic_bytes() {
        assert_eq!(
            image_format(&[0x89, b'P', b'N', b'G', 0, 0]),
            Some(ImageFormat::Png)
        );
        assert_eq!(
            image_format(&[0xFF, 0xD8, 0xFF, 0xE0]),
            Some(ImageFormat::Jpeg)
        );
        assert_eq!(image_format(b"not an image"), None);
    }
}
