//! The cell grid: its measure from the font, its place in the element, and the painting of a frame's rows (ported
//! from the old `but-gpui` view, 18 §4.2; 21 §4.7). One canvas per terminal: all backgrounds first (runs of one
//! colour merged into one quad), then one shaped line per run held to the cells by `force_width`, so a run is shaped
//! again only when it changes (GPUI's line-layout cache), then underlines and strike-throughs.

use std::hash::{Hash as _, Hasher as _};
use std::ops::Range;
use std::sync::Arc;

use but_tty::{RgbColor, Row, Run, Size, Underline, mouse};
use gpui_kit::{
    App, Bounds, Font, FontWeight, Pixels, Point, StrikethroughStyle, TextAlign, TextRun,
    UnderlineStyle, Window, point, px, size,
};

use super::colors::hsla;

/// The size of one cell and the font metrics the rows are drawn with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct CellMetrics {
    pub cell: gpui_kit::Size<Pixels>,
    pub ascent: Pixels,
    pub descent: Pixels,
}

impl CellMetrics {
    /// The cell of `font` at `font_size`: as wide as its `m`, as tall as its ascent and descent to the device pixel,
    /// so box drawing joins up from row to row (as in Ghostty).
    pub fn measure(font: &Font, font_size: Pixels, window: &Window) -> Self {
        let text_system = window.text_system();
        let font_id = text_system.resolve_font(font);
        let width = text_system
            .advance(font_id, font_size, 'm')
            .map_or(font_size * 0.6, |advance| advance.width);
        let ascent = text_system.ascent(font_id, font_size);
        let descent = text_system.descent(font_id, font_size).abs();
        let scale = window.scale_factor();
        let height = ((ascent + descent) * scale).round() / scale;
        CellMetrics {
            cell: size(width, height),
            ascent,
            descent,
        }
    }
}

/// The grid size that fits `area` with cells of `cell` (at least 2 columns and 1 row), with the cell's whole-pixel
/// size for the programs that ask.
pub(super) fn grid_size(area: gpui_kit::Size<Pixels>, cell: gpui_kit::Size<Pixels>) -> Size {
    let fit = |space: Pixels, step: Pixels| {
        if step <= px(0.) {
            return 0;
        }
        (space / step).floor().clamp(0., f32::from(u16::MAX)) as u16
    };
    Size {
        cols: fit(area.width, cell.width).max(2),
        rows: fit(area.height, cell.height).max(1),
        cell_width: f32::from(cell.width).round().clamp(1., f32::from(u16::MAX)) as u16,
        cell_height: f32::from(cell.height)
            .round()
            .clamp(1., f32::from(u16::MAX)) as u16,
    }
}

/// Where the grid was last drawn in the window, and the size its cells were drawn at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Grid {
    pub origin: Point<Pixels>,
    pub cell: gpui_kit::Size<Pixels>,
}

impl Grid {
    /// Cells from the top left of `bounds`, to the device pixel, so what whole cells leave over falls below and to
    /// the right, as Ghostty lays them out by default (dragging the panel's edge then moves no text).
    pub fn at(bounds: Bounds<Pixels>, cell: gpui_kit::Size<Pixels>, scale: f32) -> Self {
        let snap = |at: Pixels| (at * scale).round() / scale;
        Self {
            origin: point(snap(bounds.origin.x), snap(bounds.origin.y)),
            cell,
        }
    }

    /// Until it's drawn, cells are the size the shell was told.
    pub fn opening(size: Size) -> Self {
        Self {
            origin: Point::default(),
            cell: gpui_kit::size(
                px(f32::from(size.cell_width)),
                px(f32::from(size.cell_height)),
            ),
        }
    }

    /// `position` in the window on the surface the encoders see, whose cells are `size`'s whole pixels rather than
    /// the fractional ones drawn.
    pub fn surface(self, position: Point<Pixels>, size: Size) -> mouse::Position {
        let at = position - self.origin;
        mouse::Position {
            x: at.x / self.cell.width * f32::from(size.cell_width),
            y: at.y / self.cell.height * f32::from(size.cell_height),
        }
    }

    /// The window position of the top left of cell (`column`, `row`).
    pub fn cell_origin(self, column: u16, row: usize) -> Point<Pixels> {
        point(
            self.origin.x + self.cell.width * f32::from(column),
            self.origin.y + self.cell.height * row as f32,
        )
    }
}

/// What the cell drawn under `position` in the window links to, on `rows` drawn on `grid`.
pub(super) fn link_at(rows: &[Arc<Row>], grid: Grid, position: Point<Pixels>) -> Option<Arc<str>> {
    let at = position - grid.origin;
    if at.x < px(0.) || at.y < px(0.) {
        return None;
    }
    let column = (at.x / grid.cell.width).floor() as u16;
    let row = rows.get((at.y / grid.cell.height).floor() as usize)?;
    let run = row.runs.iter().find(|run| run.columns.contains(&column))?;
    run.link.clone()
}

/// The columns of `row` with a background, those side by side in one colour joined so no seam shows between them.
pub(super) fn backgrounds(row: &Row) -> impl Iterator<Item = (Range<u16>, RgbColor)> + '_ {
    let mut runs = row
        .runs
        .iter()
        .filter_map(|run| Some((run.columns.clone(), run.style.bg?)))
        .peekable();
    std::iter::from_fn(move || {
        let (mut columns, color) = runs.next()?;
        while let Some((next, _)) =
            runs.next_if(|(next, c)| *c == color && next.start == columns.end)
        {
            columns.end = next.end;
        }
        Some((columns, color))
    })
}

/// The text each run of `row` shows, with the column it starts at. Blanks at either end are left to the
/// background, so a run shows the same text however wide the row is, unless they're underlined or struck through.
pub(super) fn shown_text(row: &Row) -> impl Iterator<Item = (u16, &str, &Run)> {
    row.runs.iter().filter_map(|run| {
        let text = row.text.get(run.text.clone())?;
        let style = &run.style;
        if style.underline != Underline::None || style.strikethrough {
            return Some((run.columns.start, text, run));
        }
        let trimmed = text.trim_start_matches(' ');
        let column = run.columns.start + (text.len() - trimmed.len()) as u16;
        let trimmed = trimmed.trim_end_matches(' ');
        (!trimmed.is_empty()).then_some((column, trimmed, run))
    })
}

/// How the rows are drawn: the fonts and size, and the link under the pointer, underlined.
pub(super) struct PaintStyle<'a> {
    pub regular: &'a Font,
    pub bold: FontWeight,
    pub font_size: Pixels,
    pub metrics: CellMetrics,
    pub link: Option<&'a str>,
}

/// Paints `rows` on `grid`: every background before any text, so no cell's covers a glyph reaching into it.
pub(super) fn paint_rows(
    rows: &[Arc<Row>],
    grid: Grid,
    style: &PaintStyle<'_>,
    window: &mut Window,
    cx: &mut App,
) {
    let cell = grid.cell;
    for (y, row) in rows.iter().enumerate() {
        for (columns, color) in backgrounds(row) {
            let extent = size(
                cell.width * f32::from(columns.end - columns.start),
                cell.height,
            );
            let bounds = Bounds::new(grid.cell_origin(columns.start, y), extent);
            window.paint_quad(gpui_kit::fill(bounds, hsla(color)));
        }
    }
    let bold = Font {
        weight: style.bold,
        ..style.regular.clone()
    };
    for (y, row) in rows.iter().enumerate() {
        for (column, text, run) in shown_text(row) {
            let run_style = &run.style;
            let mut color = hsla(run_style.fg);
            if run_style.faint {
                color.fade_out(0.4);
            }
            let text_run = TextRun {
                len: text.len(),
                font: if run_style.bold { &bold } else { style.regular }.clone(),
                color,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            // Keyed by the text's hash: an unchanged run hits the layout cache without allocating its text.
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            text.hash(&mut hasher);
            // Held to the cells, as a glyph from a fallback font may be wider or narrower.
            let line = window.text_system().shape_line_by_hash(
                hasher.finish(),
                text.len(),
                style.font_size,
                &[text_run],
                Some(cell.width),
                || text.to_owned().into(),
            );
            let origin = grid.cell_origin(column, y);
            line.paint(origin, cell.height, TextAlign::Left, None, window, cx)
                .ok();
            // Across the run's cells, where GPUI's own reach only as far as the glyphs do.
            let width = cell.width * f32::from(run.columns.end.saturating_sub(column));
            let underline = match run_style.underline {
                Underline::None if style.link.is_some() && run.link.as_deref() == style.link => {
                    Underline::Single
                }
                underline => underline,
            };
            let (ascent, descent) = (style.metrics.ascent, style.metrics.descent);
            if underline != Underline::None {
                let offset = gpui_kit::underline_y_offset(cell.height, ascent, descent);
                let underline_style = UnderlineStyle {
                    thickness: px(1.),
                    color: Some(color),
                    wavy: underline == Underline::Curly,
                };
                window.paint_underline(origin + point(px(0.), offset), width, &underline_style);
            }
            if run_style.strikethrough {
                let baseline = (cell.height - ascent - descent) / 2. + ascent;
                let offset = (ascent * 0.5 + baseline) * 0.5;
                let strikethrough = StrikethroughStyle {
                    thickness: px(1.),
                    color: Some(color),
                };
                window.paint_strikethrough(origin + point(px(0.), offset), width, &strikethrough);
            }
        }
    }
}
