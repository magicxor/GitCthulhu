//! Bounds and layout assertion helpers for headless UI tests (spec 21 §8.1).
//!
//! Helpers that no test of this crate uses yet are for the region packages.
//!
//! Elements are observed through `widgets::observed(id)` (`.id(id).test_support()`); ids must be unique in the frame.

#![allow(dead_code)]

use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AnyElement, AnyWindowHandle, AppContext as _, Bounds, Context, ElementId, Entity, IntoElement,
    Pixels, Point, Render, TestAppContext, Window, WindowBounds, WindowOptions, point, px, size,
};

/// Tolerance of the layout assertions in pixels.
pub const TOLERANCE: f32 = 0.5;

/// Bounds of the observed element `id` in the last rendered frame.
pub fn bounds(window: &Window, id: impl Into<ElementId>) -> Bounds<Pixels> {
    window.find(id).bounds()
}

fn near(actual: Pixels, expected: f32) -> bool {
    (actual.as_f32() - expected).abs() <= TOLERANCE
}

/// Asserts the exact rectangle of `id` (within half a pixel).
#[track_caller]
pub fn assert_rect(window: &Window, id: impl Into<ElementId>, x: f32, y: f32, w: f32, h: f32) {
    let id = id.into();
    let b = bounds(window, id.clone());
    assert!(
        near(b.origin.x, x)
            && near(b.origin.y, y)
            && near(b.size.width, w)
            && near(b.size.height, h),
        "{id:?} is {b:?}, expected x {x} y {y} w {w} h {h}"
    );
}

/// Asserts the height of `id`.
#[track_caller]
pub fn assert_height(window: &Window, id: impl Into<ElementId>, h: f32) {
    let id = id.into();
    let b = bounds(window, id.clone());
    assert!(
        near(b.size.height, h),
        "{id:?} has height {}, expected {h}",
        b.size.height.as_f32()
    );
}

/// Asserts the width of `id`.
#[track_caller]
pub fn assert_width(window: &Window, id: impl Into<ElementId>, w: f32) {
    let id = id.into();
    let b = bounds(window, id.clone());
    assert!(
        near(b.size.width, w),
        "{id:?} has width {}, expected {w}",
        b.size.width.as_f32()
    );
}

/// Asserts that `a` ends `gap` pixels before `b` starts horizontally.
#[track_caller]
pub fn assert_left_of(window: &Window, a: impl Into<ElementId>, b: impl Into<ElementId>, gap: f32) {
    let (a, b) = (a.into(), b.into());
    let (ba, bb) = (bounds(window, a.clone()), bounds(window, b.clone()));
    let actual = bb.origin.x.as_f32() - (ba.origin.x + ba.size.width).as_f32();
    assert!(
        (actual - gap).abs() <= TOLERANCE,
        "{a:?} ({ba:?}) and {b:?} ({bb:?}) are {actual} apart horizontally, expected {gap}"
    );
}

/// Asserts that `a` ends `gap` pixels above where `b` starts.
#[track_caller]
pub fn assert_above(window: &Window, a: impl Into<ElementId>, b: impl Into<ElementId>, gap: f32) {
    let (a, b) = (a.into(), b.into());
    let (ba, bb) = (bounds(window, a.clone()), bounds(window, b.clone()));
    let actual = bb.origin.y.as_f32() - (ba.origin.y + ba.size.height).as_f32();
    assert!(
        (actual - gap).abs() <= TOLERANCE,
        "{a:?} ({ba:?}) and {b:?} ({bb:?}) are {actual} apart vertically, expected {gap}"
    );
}

/// Asserts that `id`, an element with a background of its own, is painted over every quad that covers all of it
/// (a dialog's surface and overlay). Hit testing alone cannot tell: a deferred popup takes the pointer even when
/// a layer of higher priority (a dialog) is painted over it.
#[track_caller]
pub fn assert_painted_on_top(window: &Window, id: impl Into<ElementId>) {
    let id = id.into();
    let b = bounds(window, id.clone());
    let scale = window.scale_factor();
    // The element's rectangle in the scene's scaled pixels, as (left, top, right, bottom).
    let edges = |o: Point<f32>, s: gpui_kit::Size<f32>| (o.x, o.y, o.x + s.width, o.y + s.height);
    let own = edges(
        point(b.origin.x.as_f32() * scale, b.origin.y.as_f32() * scale),
        size(
            b.size.width.as_f32() * scale,
            b.size.height.as_f32() * scale,
        ),
    );
    let tolerance = TOLERANCE * scale;
    let quads: Vec<_> = window
        .painted_quads()
        .into_iter()
        .map(|q| {
            let r = edges(
                point(q.bounds.origin.x.0, q.bounds.origin.y.0),
                size(q.bounds.size.width.0, q.bounds.size.height.0),
            );
            (q.order, r)
        })
        .collect();
    let same = |r: (f32, f32, f32, f32)| {
        (r.0 - own.0).abs() <= tolerance
            && (r.1 - own.1).abs() <= tolerance
            && (r.2 - own.2).abs() <= tolerance
            && (r.3 - own.3).abs() <= tolerance
    };
    let covers = |r: (f32, f32, f32, f32)| {
        r.0 <= own.0 + tolerance
            && r.1 <= own.1 + tolerance
            && r.2 >= own.2 - tolerance
            && r.3 >= own.3 - tolerance
    };
    let painted = quads
        .iter()
        .filter(|(_, r)| same(*r))
        .map(|(order, _)| *order)
        .max()
        .unwrap_or_else(|| panic!("{id:?} ({b:?}) paints no background of its own"));
    let over: Vec<_> = quads
        .iter()
        .filter(|(order, r)| *order > painted && covers(*r) && !same(*r))
        .collect();
    assert!(
        over.is_empty(),
        "{id:?} ({b:?}) is painted under quads covering it (order, scaled edges): {over:?}"
    );
}

/// Initialises everything the visual foundation needs in a test app: the kit, the embedded fonts, default settings
/// and the Dark theme. G0b's `init_app` builds on this.
pub fn init_visual(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_kit::init(cx);
        crate::fonts::register(cx);
        let settings = crate::settings::Settings::default();
        crate::settings::init(cx, settings.clone());
        crate::theme::init(cx, &settings);
        crate::theme::observe(cx);
    });
}

/// Window options for a headless test window of `w` x `h` logical pixels at the origin.
pub fn window_options(w: f32, h: f32) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin: point(px(0.), px(0.)),
            size: size(px(w), px(h)),
        })),
        ..Default::default()
    }
}

/// A view that renders whatever its builder returns; lets tests mount a widget without a dedicated view type.
pub struct Probe {
    build: Box<dyn FnMut(&mut Window, &mut Context<Probe>) -> AnyElement>,
}

impl Render for Probe {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        (self.build)(window, cx)
    }
}

/// Opens a `w` x `h` window that shows the element built by `build` on every frame.
pub fn open_probe(
    cx: &mut TestAppContext,
    w: f32,
    h: f32,
    build: impl FnMut(&mut Window, &mut Context<Probe>) -> AnyElement + 'static,
) -> (AnyWindowHandle, Entity<Probe>) {
    cx.update(|cx| {
        gpui_kit::open_window(window_options(w, h), cx, |_, cx| {
            cx.new(|_| Probe {
                build: Box::new(build),
            })
        })
        .expect("open the test window")
    })
}
