//! Embedded fonts: Inter (UI) and Geist Mono Nerd Font Mono (code), registered before the first window opens
//! (spec 21 §1.3, §3.4).

use std::borrow::Cow;

use gpui_kit::App;

/// Family name of the UI font as reported inside the Inter files.
pub const FONT_UI: &str = "Inter";
/// Family name of the monospace font as reported inside the Geist Mono files.
pub const FONT_MONO: &str = "GeistMono NFM";

const FONT_FILES: [&[u8]; 5] = [
    include_bytes!("../assets/fonts/Inter-Regular.ttf"),
    include_bytes!("../assets/fonts/Inter-Medium.ttf"),
    include_bytes!("../assets/fonts/Inter-SemiBold.ttf"),
    include_bytes!("../assets/fonts/GeistMonoNerdFontMono-Regular.otf"),
    include_bytes!("../assets/fonts/GeistMonoNerdFontMono-SemiBold.otf"),
];

/// Registers the embedded fonts with the text system. Invalidates the line-layout caches, so call it before
/// anything is measured. A failure is logged and the system fonts are used instead.
pub fn register(cx: &mut App) {
    if let Err(err) = try_register(cx) {
        tracing::error!("cannot register the embedded fonts: {err}");
    }
}

/// Like [`register`] but reports the failure.
pub fn try_register(cx: &mut App) -> gpui_kit::Result<()> {
    let fonts: Vec<Cow<'static, [u8]>> = FONT_FILES.iter().map(|f| Cow::Borrowed(*f)).collect();
    cx.text_system().add_fonts(fonts)
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use gpui_kit::TestAppContext;

    use super::*;

    #[gpui_kit::test]
    fn fonts_registered_inter_and_geist(cx: &mut TestAppContext) {
        // The headless test platform has no font database, so the family names cannot be listed; the embedded
        // files must still be accepted by the text system and carry the family names the theme refers to.
        cx.update(|cx| {
            try_register(cx).expect("the text system accepts the embedded fonts");
        });
        let contains = |font: &[u8], family: &str| {
            let utf16: Vec<u8> = family.encode_utf16().flat_map(u16::to_be_bytes).collect();
            font.windows(family.len()).any(|w| w == family.as_bytes())
                || font.windows(utf16.len()).any(|w| w == utf16.as_slice())
        };
        assert!(
            contains(FONT_FILES[0], FONT_UI),
            "Inter-Regular names the family `Inter`"
        );
        assert!(
            contains(FONT_FILES[3], FONT_MONO),
            "GeistMono Regular names the family `GeistMono NFM`"
        );
    }
}
