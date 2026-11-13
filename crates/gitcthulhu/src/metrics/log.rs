//! Layout constants of the log (owned by G4 after G0b; 08 §2). All values in dp.

// graph column - 08 §2.1, §2.2, §2.5, §2.6
/// Minimum width of the graph column.
pub const GRAPH_MIN_W: f32 = 100.; // 08 §2.1 CANVAS_MIN_WIDTH
/// Default width of the graph column (no drag padding).
pub const GRAPH_DEFAULT_W: f32 = 120.; // 08 §2.1 CANVAS_DEFAULT_WIDTH
/// Lanes assumed for the scroll width while no commit is loaded.
pub const MIN_GRAPH_LANES: u16 = 2; // 08 §2.1
/// Extra lanes added to the scroll width of the graph.
pub const MARGIN_GRAPH_LANES: u16 = 2; // 08 §2.1
/// Width of the graph divider's hit area (header and list).
pub const DIVIDER_W: f32 = 8.; // 08 §2.1 DIVIDER_WIDTH
/// Width of the visible line inside the divider.
pub const DIVIDER_LINE_W: f32 = 2.; // 08 §2.4, §2.5
/// Horizontal wheel delta multiplier of the graph column.
pub const HSCROLL_MULTIPLIER: f32 = 10.; // 08 §2.1
/// Inset of the horizontal graph scrollbar from the bottom-left corner.
pub const HSCROLL_INSET: f32 = 4.; // 08 §2.2
/// Bottom padding after the last row.
pub const BOTTOM_PADDING: f32 = 80.; // 08 §2.1 LOG_BOTTOM_PADDING
/// Graph strokes (lines, arcs, node borders).
pub const STROKE: f32 = 2.; // 08 §2.1
/// Offset of the fork and merge horizontals when a row has both.
pub const FORK_MERGE_OFFSET: f32 = 2.; // 08 §2.9

// nodes - 08 §2.8, §2.9
/// The node box (avatar, stash and uncommitted nodes).
pub const NODE: f32 = 30.; // 08 §2.9
/// Corner radius of a merge commit's rounded-square avatar.
pub const MERGE_NODE_RADIUS: f32 = 4.; // 08 §2.9
/// The stash glyph inside a stash node.
pub const STASH_ICON: f32 = 20.; // 08 §2.9

// header and search - 08 §2.3, §2.4
/// Height of the search bar above the header.
pub const SEARCH_BAR_H: f32 = 56.; // 08 §2.3
/// Start padding of the "Graph" and "Message" header labels.
pub const HEADER_LABEL_PAD: f32 = 16.; // 08 §2.4
/// End padding of the header's search button.
pub const HEADER_SEARCH_PAD_END: f32 = 8.; // 08 §2.4
/// Icon size of the search bar's arrow and close buttons (Material default).
pub const SEARCH_ICON: f32 = 24.; // 08 §2.3
/// End padding of the search bar's close button.
pub const SEARCH_CLOSE_PAD_END: f32 = 4.; // 08 §2.3
/// Horizontal padding of the search field's text (Material filled text field).
pub const SEARCH_FIELD_PAD_H: f32 = 16.; // 21 §4.6 (filled field)
/// Bottom indicator of the search field: unfocused / focused.
pub const SEARCH_INDICATOR: f32 = 1.; // 08 §2.3
pub const SEARCH_INDICATOR_FOCUSED: f32 = 2.; // 08 §2.3
/// Top of the search field's label: floating (focused or non-empty) / resting.
pub const SEARCH_LABEL_TOP_FLOATING: f32 = 8.; // 21 §4.6 (filled field)
pub const SEARCH_LABEL_TOP: f32 = 18.; // 21 §4.6 (filled field, centred in 56)
/// Top of the search field's text.
pub const SEARCH_INPUT_TOP: f32 = 24.; // 21 §4.6 (filled field)

// rows - 08 §2.7
/// Start padding of the chip row in front of the message.
pub const CHIPS_PAD_START: f32 = 16.; // 08 §2.7.2
/// Start padding of the message text.
pub const MESSAGE_PAD_START: f32 = 8.; // 08 §2.7.2
/// Horizontal padding of the date.
pub const DATE_PAD_H: f32 = 16.; // 08 §2.7.2
/// End padding of a commit row's message area.
pub const ROW_PAD_END: f32 = 4.; // 08 §2.7.1
/// Start offset of the search match bar inside the message area.
pub const MATCH_BAR_START: f32 = 8.; // 08 §2.7.1
/// Width of the search match bar.
pub const MATCH_BAR_W: f32 = 4.; // 08 §2.7.1

// uncommitted row - 08 §2.8
/// Padding of the uncommitted row's message area (all sides).
pub const UNCOMMITTED_PAD: f32 = 8.; // 08 §2.8
/// Start padding of the uncommitted label.
pub const UNCOMMITTED_LABEL_PAD: f32 = 16.; // 08 §2.8
/// Horizontal padding of the status summary.
pub const SUMMARY_PAD_H: f32 = 8.; // 08 §2.8
/// Horizontal padding of one summary entry.
pub const SUMMARY_ENTRY_PAD_H: f32 = 4.; // 08 §2.8
/// Icon of a summary entry.
pub const SUMMARY_ICON: f32 = 14.; // 08 §2.8

// chips - 08 §2.12
/// Outer horizontal margin of a chip.
pub const CHIP_MARGIN_H: f32 = 4.; // 08 §2.12
/// Height of a chip (= its icon box).
pub const CHIP_H: f32 = 26.; // 08 §2.12
/// Corner radius of a chip.
pub const CHIP_RADIUS: f32 = 16.; // 08 §2.12
/// Border of a chip (drawn over the content edge).
pub const CHIP_BORDER: f32 = 2.; // 08 §2.12
/// The chip's icon.
pub const CHIP_ICON: f32 = 14.; // 08 §2.12
/// Horizontal padding of the chip's name.
pub const CHIP_TEXT_PAD_H: f32 = 6.; // 08 §2.12
/// The current-branch indicator box and its end padding.
pub const CHIP_CURRENT: f32 = 18.; // 08 §2.12
pub const CHIP_CURRENT_PAD_END: f32 = 6.; // 08 §2.12
/// The filled dot drawn inside the current-branch indicator box.
pub const CHIP_CURRENT_DOT: f32 = 10.; // 08 §2.12 (location glyph)

// scroll pill - 08 §2.10
/// Top offset of the "Scroll to top" pill.
pub const PILL_TOP: f32 = 16.; // 08 §2.10
/// Padding of the pill: vertical / horizontal.
pub const PILL_PAD_V: f32 = 4.; // 08 §2.10
pub const PILL_PAD_H: f32 = 8.; // 08 §2.10
/// The pill's icon and the gap before its text.
pub const PILL_ICON: f32 = 20.; // 08 §2.10
pub const PILL_GAP: f32 = 8.; // 08 §2.10
