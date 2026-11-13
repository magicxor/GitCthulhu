use core::prelude::v1::test;

use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, Modifiers, TestAppContext};

use super::doc::{BlameDoc, UNCOMMITTED_LABEL};
use super::ids;
use crate::backend::{BlameChunk, BlameCommit, BlameResult, BlameRev, ObjectId, RelaPath};
use crate::metrics::blame as m;
use crate::tab::RepoTab;
use crate::tab::ui::{CloseableView, FileList, Selection};
use crate::test_support::geometry::{assert_height, assert_width, bounds};
use crate::test_support::{DEFAULT_SIZE, Fixture, mount_fixture, toast_texts};

const FILE: &str = "src/lib.rs";

/// A repository whose file has two committed revisions and one uncommitted line:
/// line 1 and 3 from "first commit", line 2 from "second commit", line 4 uncommitted.
fn fixture() -> (Fixture, String, String) {
    let f = Fixture::new();
    f.write(FILE, "line one\nline two\nline three\n");
    f.commit_all("first commit");
    let first = f.git(&["rev-parse", "HEAD"]).trim().to_owned();
    f.write(FILE, "line one\nline TWO\nline three\n");
    f.commit_all("second commit");
    let second = f.git(&["rev-parse", "HEAD"]).trim().to_owned();
    f.write(FILE, "line one\nline TWO\nline three\nline five\n");
    (f, first, second)
}

fn open(
    cx: &mut TestAppContext,
    handle: AnyWindowHandle,
    tab: &Entity<RepoTab>,
    path: &str,
    rev: BlameRev,
) {
    let ctx = cx.update(|cx| tab.read(cx).ctx().clone());
    cx.update(|cx| super::open(&ctx, RelaPath::from(path), rev, cx));
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .expect("window");
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .expect("window");
}

/// Spec 09 §2.1: header 40, gutter + 200 dp commit cell + code, rows 34; opening a single-file diff minimizes the
/// blame into the 52 dp bar below the log/diff column (09 §1.1).
#[gpui_kit::test]
fn blame_layout_and_minimize_on_diff_open(cx: &mut TestAppContext) {
    let (f, _, _) = fixture();
    let (handle, _app, tab) = mount_fixture(cx, &f, DEFAULT_SIZE);
    open(cx, handle, &tab, FILE, BlameRev::Worktree);

    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let root = bounds(window, ids::BLAME);
        // Literal spec values (09 §2.1): header 40 with padding 8 + 16 before the path and 8 after the close
        // button; commit cell 200; rows ≈33 in Gitnuro, fixed 34 here (21 §4.0).
        assert_height(window, ids::BLAME_HEADER, 40.);
        let title = bounds(window, ids::BLAME_TITLE);
        assert!(
            ((title.left() - root.left()).as_f32() - 8.).abs() < 0.5,
            "the title box (with its 16 dp padding) starts after the bar's 8 dp padding: {:?} vs {:?}",
            title.left(),
            root.left()
        );
        let close = bounds(window, ids::BLAME_CLOSE);
        assert!(
            ((root.right() - close.right()).as_f32() - 8.).abs() < 0.5,
            "close button ends 8 dp from the right edge"
        );
        assert_height(window, ids::BLAME_CLOSE, 40.);
        assert_width(window, ids::BLAME_CLOSE, 48.);
        assert_height(window, (ids::BLAME_COMMIT, 0usize), 34.);
        assert_width(window, (ids::BLAME_COMMIT, 0usize), 200.);
        assert_height(window, ids::BLAME_HEADER, m::HEADER_H);
        assert_width(window, ids::BLAME_HEADER, root.size.width.as_f32());
        let header = bounds(window, ids::BLAME_HEADER);
        assert_eq!(header.origin, root.origin, "the header is at the top");

        let gutter = bounds(window, (ids::BLAME_GUTTER, 0usize));
        let commit = bounds(window, (ids::BLAME_COMMIT, 0usize));
        let code = bounds(window, (ids::BLAME_CODE, 0usize));
        assert_height(window, (ids::BLAME_COMMIT, 0usize), m::ROW_H);
        assert_width(window, (ids::BLAME_COMMIT, 0usize), m::COMMIT_CELL_W);
        assert!(
            (gutter.right() - commit.left()).abs().as_f32() < 0.5,
            "no gap between gutter and commit cell"
        );
        assert!(
            (commit.right() - code.left()).abs().as_f32() < 0.5,
            "the code starts after the commit cell"
        );
        assert!(
            (code.right() - root.right()).abs().as_f32() < 0.5,
            "the code column fills the rest"
        );
        assert!(
            (bounds(window, (ids::BLAME_COMMIT, 1usize)).top() - commit.bottom())
                .abs()
                .as_f32()
                < 0.5,
            "rows are stacked without gaps"
        );
        assert!(
            (gutter.size.width.as_f32() - (8.4 + 8.)).abs() < 1.5,
            "one digit of 14 sp mono plus 4 + 4 padding, got {}",
            gutter.size.width.as_f32()
        );
    })
    .expect("window");

    // The full blame replaces the log: no log in the frame.
    cx.update_window(handle, |_, window, _| {
        assert!(window.try_find(crate::tab::ids::CENTER_LOG).is_none());
    })
    .expect("window");

    // Clicking one unstaged file opens its diff and minimizes the blame.
    let ui = cx.update(|cx| tab.read(cx).ctx().ui.clone());
    ui.update(cx, |ui, cx| {
        ui.click_file(
            FileList::Unstaged,
            RelaPath::from(FILE),
            Modifiers::default(),
            &[RelaPath::from(FILE)],
            cx,
        )
    });
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
        let bar = bounds(window, ids::BLAME);
        assert_height(window, ids::BLAME, 52.);
        assert_height(window, ids::BLAME, m::MINIMIZED_H);
        let diff = bounds(window, crate::tab::ids::CENTER_DIFF);
        assert!(
            (diff.bottom() - bar.top()).abs().as_f32() < 0.5,
            "the bar sits right below the diff"
        );
        assert!(window.try_find(ids::BLAME_HEADER).is_none());
        assert!(window.try_find(ids::BLAME_SHOW).is_some());
    })
    .expect("window");
    cx.update(|cx| {
        let blame = ui.read(cx).blame.clone().expect("still open");
        assert!(blame.minimized);
    });

    // "Show" expands it again; the diff is still selected but the blame wins the centre pane.
    cx.update_window(handle, |_, window, cx| window.click(ids::BLAME_SHOW, cx))
        .expect("window");
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
        assert!(window.try_find(ids::BLAME_HEADER).is_some());
    })
    .expect("window");
    cx.update(|cx| assert!(!ui.read(cx).blame.as_ref().expect("open").minimized));

    // Closing from the header drops the view and its Esc entry.
    cx.update_window(handle, |_, window, cx| window.click(ids::BLAME_CLOSE, cx))
        .expect("window");
    cx.update(|cx| {
        let ui = ui.read(cx);
        assert!(ui.blame.is_none());
        assert!(!ui.views.contains(CloseableView::Blame));
    });
}

/// Spec 09 §2.1: a click on the commit cell selects the commit in the log and the right pane; the blame stays.
#[gpui_kit::test]
fn blame_row_click_selects_commit(cx: &mut TestAppContext) {
    let (f, first, second) = fixture();
    let (handle, _app, tab) = mount_fixture(cx, &f, DEFAULT_SIZE);
    open(cx, handle, &tab, FILE, BlameRev::Worktree);
    let ui = cx.update(|cx| tab.read(cx).ctx().ui.clone());

    let selected = |cx: &mut TestAppContext| cx.update(|cx| ui.read(cx).selection.clone());
    let hex = |id: ObjectId| id.to_string();

    cx.update_window(handle, |_, window, cx| {
        window.click((ids::BLAME_COMMIT, 1usize), cx)
    })
    .expect("window");
    match selected(cx) {
        Selection::Commit {
            id,
            is_stash,
            scroll_to,
        } => {
            assert_eq!(
                hex(id),
                second,
                "line 2 was last changed by the second commit"
            );
            assert!(!is_stash && scroll_to);
        }
        other => panic!("expected a commit selection, got {other:?}"),
    }
    cx.update_window(handle, |_, window, cx| {
        window.click((ids::BLAME_COMMIT, 0usize), cx)
    })
    .expect("window");
    match selected(cx) {
        Selection::Commit { id, .. } => assert_eq!(hex(id), first),
        other => panic!("expected a commit selection, got {other:?}"),
    }
    cx.update(|cx| assert!(ui.read(cx).blame.is_some(), "the blame stays open"));
}

/// Spec 09 §2.1: lines that are not committed show "Uncommitted change"; their click selects the uncommitted
/// changes.
#[gpui_kit::test]
fn uncommitted_lines_label(cx: &mut TestAppContext) {
    let (f, _, _) = fixture();
    let (handle, _app, tab) = mount_fixture(cx, &f, DEFAULT_SIZE);
    open(cx, handle, &tab, FILE, BlameRev::Worktree);
    let ui = cx.update(|cx| tab.read(cx).ctx().ui.clone());
    let view = cx.update(|cx| tab.read(cx).views.blame.clone().expect("blame view"));
    cx.update(|cx| {
        let doc = view.read(cx).doc().expect("loaded");
        assert_eq!(doc.line_count(), 4);
        assert_eq!(doc.line_text(3), "line five");
        assert!(doc.commit_of(3).is_none(), "the new line has no commit");
        assert!(doc.commit_of(2).is_some());
    });
    assert_eq!(UNCOMMITTED_LABEL, "Uncommitted change");

    cx.update_window(handle, |_, window, cx| {
        window.click((ids::BLAME_COMMIT, 0usize), cx);
        window.click((ids::BLAME_COMMIT, 3usize), cx);
    })
    .expect("window");
    cx.update(|cx| assert_eq!(ui.read(cx).selection, Selection::UncommittedChanges));
}

/// Spec 09 §2.1: Esc closes the blame (it is a closeable view).
#[gpui_kit::test]
fn escape_closes_blame(cx: &mut TestAppContext) {
    let (f, _, _) = fixture();
    let (handle, _app, tab) = mount_fixture(cx, &f, DEFAULT_SIZE);
    open(cx, handle, &tab, FILE, BlameRev::Worktree);
    let ui = cx.update(|cx| tab.read(cx).ctx().ui.clone());
    cx.update(|cx| assert!(ui.read(cx).views.contains(CloseableView::Blame)));
    cx.update_window(handle, |_, window, cx| {
        tab.update(cx, |t, cx| t.focus(window, cx));
        window.render_frame(cx);
        window.press("escape", cx);
    })
    .expect("window");
    cx.update(|cx| assert!(ui.read(cx).blame.is_none()));
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .expect("window");
    cx.update(|cx| assert!(tab.read(cx).views.blame.is_none(), "the view is dropped"));
}

/// Spec 09 §2.1: a failing blame resets the state and reports "File blaming failed".
#[gpui_kit::test]
fn failed_blame_closes_with_a_toast(cx: &mut TestAppContext) {
    let (f, _, _) = fixture();
    let (handle, app, tab) = mount_fixture(cx, &f, DEFAULT_SIZE);
    open(cx, handle, &tab, "no/such/file.rs", BlameRev::Worktree);
    cx.run_until_parked();
    let ui = cx.update(|cx| tab.read(cx).ctx().ui.clone());
    cx.update(|cx| {
        assert!(ui.read(cx).blame.is_none(), "the blame was closed");
        assert!(!ui.read(cx).views.contains(CloseableView::Blame));
        let texts = toast_texts(&app, cx);
        assert!(
            texts.iter().any(|t| t.starts_with("File blaming failed")),
            "toasts: {texts:?}"
        );
    });
}

/// Spec 09 §2.1: a blame at a commit shows the file as of that commit.
#[gpui_kit::test]
fn blame_at_a_commit(cx: &mut TestAppContext) {
    let (f, first, _) = fixture();
    let (handle, _app, tab) = mount_fixture(cx, &f, DEFAULT_SIZE);
    let id = ObjectId::from_hex(first.as_bytes()).expect("a hex id");
    open(cx, handle, &tab, FILE, BlameRev::Commit(id));
    let view = cx.update(|cx| tab.read(cx).views.blame.clone().expect("blame view"));
    cx.update(|cx| {
        let doc = view.read(cx).doc().expect("loaded");
        assert_eq!(doc.line_count(), 3);
        assert_eq!(doc.line_text(1), "line two", "the first revision of line 2");
        assert!(
            (0..3).all(|l| doc.commit_of(l).is_some_and(|c| c.id == id)),
            "every line is from the first commit"
        );
    });
}

/// 21 §4.0: the list is virtualised; a 5,000 line file renders only the visible rows.
#[gpui_kit::test]
fn long_file_renders_only_visible_rows(cx: &mut TestAppContext) {
    let f = Fixture::new();
    let text: String = (0..5_000)
        .map(|i| format!("let value_{i} = {i};\n"))
        .collect();
    f.write("big.rs", &text);
    f.commit_all("big");
    let (handle, _app, tab) = mount_fixture(cx, &f, DEFAULT_SIZE);
    open(cx, handle, &tab, "big.rs", BlameRev::Worktree);
    cx.update_window(handle, |_, window, _| {
        assert!(window.try_find((ids::BLAME_COMMIT, 0usize)).is_some());
        assert!(
            window.try_find((ids::BLAME_COMMIT, 4_000usize)).is_none(),
            "rows far below the viewport do not exist"
        );
        let rendered = (0..200usize)
            .filter(|ix| window.try_find((ids::BLAME_COMMIT, *ix)).is_some())
            .count();
        assert!(
            (10..=40).contains(&rendered),
            "only a viewport of rows is built, got {rendered}"
        );
    })
    .expect("window");
}

fn label_doc() -> BlameDoc {
    use crate::backend::{GitTime, Person, Signature};
    let person = std::sync::Arc::new(Person {
        name: "Ada".into(),
        email: "ada@example.com".into(),
    });
    let commit = |n: u8| BlameCommit {
        id: ObjectId::from_hex(format!("{n:040x}").as_bytes()).expect("id"),
        author: Signature {
            person: person.clone(),
            time: GitTime {
                seconds: 0,
                offset_minutes: 0,
            },
        },
        summary: format!("commit {n}"),
        source_path: RelaPath::from("f"),
    };
    let text = "a\nb\nc\nd\ne\n".to_owned();
    BlameDoc::new(BlameResult {
        path: RelaPath::from("f"),
        rev: BlameRev::Worktree,
        commits: vec![commit(1), commit(2)],
        chunks: vec![
            BlameChunk {
                start_line: 0,
                len: 2,
                commit: Some(0),
                source_start_line: 0,
            },
            BlameChunk {
                start_line: 2,
                len: 1,
                commit: None,
                source_start_line: 0,
            },
            BlameChunk {
                start_line: 3,
                len: 2,
                commit: Some(1),
                source_start_line: 0,
            },
        ],
        lines: (0..5).map(|i| (i * 2)..(i * 2 + 1)).collect(),
        text,
    })
}

#[test]
fn chunk_lookup_by_line() {
    let doc = label_doc();
    let owners: Vec<Option<String>> = (0..6)
        .map(|l| doc.commit_of(l).map(|c| c.summary.to_string()))
        .collect();
    assert_eq!(
        owners,
        [
            Some("commit 1".to_owned()),
            Some("commit 1".to_owned()),
            None,
            Some("commit 2".to_owned()),
            Some("commit 2".to_owned()),
            None,
        ],
        "line 5 is past the end"
    );
    assert_eq!(doc.digits, 1);
    assert_eq!(doc.full_text(), "a\nb\nc\nd\ne");
    assert_eq!(&*doc.labels[0].author, "Ada");
}

/// Spec 09 §2.1: the code column is selectable; copying joins the selected lines with newlines.
#[gpui_kit::test]
fn code_column_selection_copies_lines(cx: &mut TestAppContext) {
    let (f, _, _) = fixture();
    let (handle, _app, tab) = mount_fixture(cx, &f, DEFAULT_SIZE);
    open(cx, handle, &tab, FILE, BlameRev::Worktree);
    let view = cx.update(|cx| tab.read(cx).views.blame.clone().expect("blame view"));
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let first = bounds(window, (ids::BLAME_CODE, 0usize));
        let third = bounds(window, (ids::BLAME_CODE, 2usize));
        let from = gpui_kit::point(first.left() + gpui_kit::px(18.), first.center().y);
        let to = gpui_kit::point(third.right() - gpui_kit::px(4.), third.center().y);
        window.drag(from, to, cx);
        window.render_frame(cx);
        let text = view.read(cx).bridge.selected_text(cx);
        assert_eq!(text, "line one\nline TWO\nline three");
        assert!(view.read(cx).bridge.has_selection(cx));
    })
    .expect("window");
}

/// Select all (context menu) copies the whole file, including lines that are scrolled out.
#[gpui_kit::test]
fn select_all_copies_the_whole_file(cx: &mut TestAppContext) {
    let f = Fixture::new();
    let text: String = (0..300).map(|i| format!("row {i}\n")).collect();
    f.write("many.txt", &text);
    f.commit_all("many");
    let (handle, _app, tab) = mount_fixture(cx, &f, DEFAULT_SIZE);
    open(cx, handle, &tab, "many.txt", BlameRev::Worktree);
    let view = cx.update(|cx| tab.read(cx).views.blame.clone().expect("blame view"));
    cx.update_window(handle, |_, window, cx| {
        let bridge = view.read(cx).bridge.clone();
        bridge.select_all(cx);
        window.render_frame(cx);
        let copied = view.read(cx).bridge.selected_text(cx);
        assert_eq!(copied, text.trim_end_matches('\n'));
    })
    .expect("window");
}

/// A selection made in a scrolled list keeps its lines when they scroll out of view (the list is virtualised).
#[gpui_kit::test]
fn selection_survives_scrolling_out(cx: &mut TestAppContext) {
    let f = Fixture::new();
    let text: String = (0..300).map(|i| format!("row {i}\n")).collect();
    f.write("many.txt", &text);
    f.commit_all("many");
    let (handle, _app, tab) = mount_fixture(cx, &f, DEFAULT_SIZE);
    open(cx, handle, &tab, "many.txt", BlameRev::Worktree);
    let view = cx.update(|cx| tab.read(cx).views.blame.clone().expect("blame view"));
    let scroll = cx.update(|cx| view.read(cx).scroll.clone());
    cx.update_window(handle, |_, window, cx| {
        scroll.scroll_to_item(5, gpui_kit::ScrollStrategy::Top);
        window.render_frame(cx);
        window.render_frame(cx);
        let first = bounds(window, (ids::BLAME_CODE, 7usize));
        let last = bounds(window, (ids::BLAME_CODE, 9usize));
        let from = gpui_kit::point(first.left() + gpui_kit::px(17.), first.center().y);
        let to = gpui_kit::point(last.right() - gpui_kit::px(4.), last.center().y);
        window.drag(from, to, cx);
        window.render_frame(cx);
        let before = view.read(cx).bridge.selected_text(cx);
        assert_eq!(before, "row 7\nrow 8\nrow 9");
        scroll.scroll_to_item(200, gpui_kit::ScrollStrategy::Top);
        window.render_frame(cx);
        window.render_frame(cx);
        assert!(
            window.try_find((ids::BLAME_CODE, 8usize)).is_none(),
            "scrolled out"
        );
        let after = view.read(cx).bridge.selected_text(cx);
        assert_eq!(after, "row 7\nrow 8\nrow 9");
    })
    .expect("window");
}

#[test]
fn syntax_tokens_become_foreground_runs() {
    use crate::diff::highlight::{LineTokens, SyntaxToken};
    let colors = crate::theme::Tokens::from_argb(&[], false);
    let mut line = LineTokens::new();
    line.push((0..2, SyntaxToken::Keyword));
    line.push((3..99, SyntaxToken::Comment));
    line.push((3..5, SyntaxToken::Annotation));
    let runs = super::rows::highlights(Some(&line), "fn x()", &colors);
    let shape: Vec<_> = runs.iter().map(|(r, s)| (r.clone(), s.color)).collect();
    assert_eq!(
        shape,
        vec![
            (0..2, Some(colors.diff_keyword)),
            (3..5, Some(colors.diff_annotation))
        ],
        "the out-of-range token is dropped"
    );
    assert!(super::rows::highlights(None, "fn x()", &colors).is_empty());
}

/// Opening the blame of another file while one is open replaces it; a worktree blame follows a new commit.
#[gpui_kit::test]
fn blame_switches_file_and_follows_commits(cx: &mut TestAppContext) {
    let (f, _, _) = fixture();
    f.write("other.txt", "alpha\nbeta\n");
    f.git(&["add", "other.txt"]);
    f.git(&["commit", "-q", "-m", "other"]);
    let (handle, _app, tab) = mount_fixture(cx, &f, DEFAULT_SIZE);
    open(cx, handle, &tab, FILE, BlameRev::Worktree);
    let view = cx.update(|cx| tab.read(cx).views.blame.clone().expect("blame view"));
    cx.update(|cx| assert_eq!(view.read(cx).doc().expect("loaded").line_count(), 4));

    open(cx, handle, &tab, "other.txt", BlameRev::Worktree);
    cx.update(|cx| {
        let v = view.read(cx);
        let doc = v.doc().expect("the other file is loaded");
        assert_eq!(doc.line_count(), 2);
        assert_eq!(doc.line_text(0), "alpha");
        assert_eq!(
            v.target().map(|(p, _)| p.clone()),
            Some(RelaPath::from("other.txt"))
        );
    });

    // Back to the first file; committing its uncommitted line and reloading the log updates the blame.
    open(cx, handle, &tab, FILE, BlameRev::Worktree);
    cx.update(|cx| assert!(view.read(cx).doc().expect("loaded").commit_of(3).is_none()));
    f.git(&["add", FILE]);
    f.git(&["commit", "-q", "-m", "fifth line"]);
    cx.update(|cx| {
        tab.update(cx, |t, cx| t.reload(crate::backend::RefreshSet::ALL, cx));
    });
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.render_frame(cx))
        .expect("window");
    cx.update(|cx| {
        let doc = view.read(cx).doc().expect("reloaded").clone();
        let owner = doc.commit_of(3).expect("the line is committed now");
        assert_eq!(&*owner.summary, "fifth line");
    });
}

/// Ctrl+C after a click into the blame copies the selected code untrimmed: the first line keeps its indentation
/// (the kit's window copy trims it).
#[gpui_kit::test]
fn ctrl_c_copies_the_selection_with_indentation(cx: &mut TestAppContext) {
    let f = Fixture::new();
    f.write(
        "main.rs",
        "fn main() {
    let x = 1;
}
",
    );
    f.commit_all("main");
    let (handle, _app, tab) = mount_fixture(cx, &f, DEFAULT_SIZE);
    open(cx, handle, &tab, "main.rs", BlameRev::Worktree);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        let line = bounds(window, (ids::BLAME_CODE, 1usize));
        let from = gpui_kit::point(line.left() + gpui_kit::px(17.), line.center().y);
        let to = gpui_kit::point(line.right() - gpui_kit::px(4.), line.center().y);
        window.drag(from, to, cx);
        window.render_frame(cx);
        window.press("secondary-c", cx);
    })
    .expect("window");
    let copied = cx.update(|cx| cx.read_from_clipboard().and_then(|c| c.text()));
    assert_eq!(copied.as_deref(), Some("    let x = 1;"));

    // Closing with X hands the keyboard back to the frame (the focused view is dropped).
    cx.update_window(handle, |_, window, cx| {
        window.click(ids::BLAME_CLOSE, cx);
        window.render_frame(cx);
        assert!(tab.read(cx).focus.contains_focused(window, cx));
    })
    .expect("window");
}

/// A blame view that is dropped by something else than its close button (here: the state is cleared) while it holds
/// the focus hands the focus back to the frame.
#[gpui_kit::test]
fn dropping_the_focused_blame_view_refocuses_the_frame(cx: &mut TestAppContext) {
    let f = Fixture::new();
    f.write("main.rs", "fn main() {}\n");
    f.commit_all("main");
    let (handle, _app, tab) = mount_fixture(cx, &f, DEFAULT_SIZE);
    open(cx, handle, &tab, "main.rs", BlameRev::Worktree);
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.click((ids::BLAME_CODE, 0usize), cx);
        window.render_frame(cx);
        assert!(window.focused(cx).is_some(), "the click focused the view");
    })
    .expect("window");
    cx.update(|cx| crate::blame::close(&tab.read(cx).ctx().clone(), cx));
    cx.update_window(handle, |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
        assert!(
            tab.read(cx).focus.contains_focused(window, cx),
            "the frame has the focus after the view went away"
        );
    })
    .expect("window");
}

/// An endpoint that scrolled out of the list still cuts its line where the selection starts or ends (the copy used
/// to take the whole line), and the selection still counts as one when every selected line is out of view.
#[gpui_kit::test]
fn partial_endpoints_survive_scrolling_out(cx: &mut TestAppContext) {
    let f = Fixture::new();
    let text: String = (0..300).map(|i| format!("row number {i}\n")).collect();
    f.write("many.txt", &text);
    f.commit_all("many");
    let (handle, _app, tab) = mount_fixture(cx, &f, DEFAULT_SIZE);
    open(cx, handle, &tab, "many.txt", BlameRev::Worktree);
    let view = cx.update(|cx| tab.read(cx).views.blame.clone().expect("blame view"));
    let scroll = cx.update(|cx| view.read(cx).scroll.clone());
    cx.update_window(handle, |_, window, cx| {
        scroll.scroll_to_item(5, gpui_kit::ScrollStrategy::Top);
        window.render_frame(cx);
        window.render_frame(cx);
        let first = bounds(window, (ids::BLAME_CODE, 7usize));
        let last = bounds(window, (ids::BLAME_CODE, 9usize));
        // From inside "row number 7" to inside "row number 9".
        let from = gpui_kit::point(first.left() + gpui_kit::px(17. + 40.), first.center().y);
        let to = gpui_kit::point(last.left() + gpui_kit::px(17. + 60.), last.center().y);
        window.drag(from, to, cx);
        window.render_frame(cx);
        let before = view.read(cx).bridge.selected_text(cx);
        let lines: Vec<&str> = before.lines().collect();
        assert_eq!(lines.len(), 3, "three lines: {before:?}");
        assert!(
            "row number 7".ends_with(lines[0]) && lines[0].len() < "row number 7".len(),
            "the first line is cut at the start: {before:?}"
        );
        assert_eq!(lines[1], "row number 8");
        assert!(
            "row number 9".starts_with(lines[2]) && lines[2].len() < "row number 9".len(),
            "the last line is cut at the end: {before:?}"
        );
        scroll.scroll_to_item(200, gpui_kit::ScrollStrategy::Top);
        window.render_frame(cx);
        window.render_frame(cx);
        assert!(
            window.try_find((ids::BLAME_CODE, 8usize)).is_none(),
            "scrolled out"
        );
        assert!(
            view.read(cx).bridge.has_selection(cx),
            "a selection that is entirely out of view still counts"
        );
        assert_eq!(
            view.read(cx).bridge.selected_text(cx),
            before,
            "the endpoints keep their offsets"
        );
    })
    .expect("window");
}
