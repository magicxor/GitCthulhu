//! Tab characters in the commit message field.
//!
//! The kit's text area shapes a tab as a glyph without width, so Git's `MERGE_MSG` lines (`#<TAB>docs/guide.md`)
//! lost their gap. The field therefore shows tabs expanded to the next multiple of [`TAB_WIDTH`] columns, and the
//! message that is committed (or saved as a draft) gets the original tab lines back: a line that still reads as
//! the expansion of a line that held tabs is replaced by that original line, a line the user edited stays as typed.

/// Columns between tab stops in the field.
pub(super) const TAB_WIDTH: usize = 4;

/// The lines of the loaded text that held tabs: `(as shown, original)`, in text order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct TabLines(Vec<(String, String)>);

/// `text` as the field shows it, and the tab lines to restore later.
pub(super) fn expand(text: &str) -> (String, TabLines) {
    if !text.contains('\t') {
        return (text.to_owned(), TabLines::default());
    }
    let mut shown = String::with_capacity(text.len() + 16);
    let mut lines = Vec::new();
    for (ix, line) in text.split('\n').enumerate() {
        if ix > 0 {
            shown.push('\n');
        }
        if line.contains('\t') {
            let expanded = expand_line(line);
            shown.push_str(&expanded);
            lines.push((expanded, line.to_owned()));
        } else {
            shown.push_str(line);
        }
    }
    (shown, TabLines(lines))
}

/// The message to commit: `current` (the field's text) with the original lines of `tabs` put back.
pub(super) fn restore(current: &str, tabs: &TabLines) -> String {
    if tabs.0.is_empty() {
        return current.to_owned();
    }
    let mut used = vec![false; tabs.0.len()];
    let mut out = String::with_capacity(current.len());
    for (ix, line) in current.split('\n').enumerate() {
        if ix > 0 {
            out.push('\n');
        }
        let original = tabs
            .0
            .iter()
            .enumerate()
            .find(|(at, (shown, _))| !used[*at] && shown == line);
        match original {
            Some((at, (_, original))) => {
                used[at] = true;
                out.push_str(original);
            }
            None => out.push_str(line),
        }
    }
    out
}

/// One line with every tab replaced by the spaces up to the next tab stop (columns count characters).
fn expand_line(line: &str) -> String {
    let mut out = String::with_capacity(line.len() + 8);
    let mut column = 0;
    for ch in line.chars() {
        if ch == '\t' {
            let fill = TAB_WIDTH - column % TAB_WIDTH;
            out.extend(std::iter::repeat_n(' ', fill));
            column += fill;
        } else {
            out.push(ch);
            column += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use core::prelude::v1::test;

    use super::*;

    const MERGE_MSG: &str =
        "Merge branch 'dev'\n\n# Conflicts:\n#\tdocs/guide.md\n#\tsrc/app.txt\n";

    #[test]
    fn tabs_are_shown_as_a_gap_up_to_the_next_stop() {
        let (shown, _) = expand(MERGE_MSG);
        assert_eq!(
            shown,
            "Merge branch 'dev'\n\n# Conflicts:\n#   docs/guide.md\n#   src/app.txt\n"
        );
        let (shown, _) = expand("ab\tc\t\td");
        assert_eq!(shown, "ab  c       d", "a second tab adds a whole stop");
    }

    #[test]
    fn text_without_tabs_is_untouched() {
        let (shown, tabs) = expand("plain\ntext");
        assert_eq!(shown, "plain\ntext");
        assert_eq!(tabs, TabLines::default());
        assert_eq!(restore("plain\ntext", &tabs), "plain\ntext");
    }

    #[test]
    fn untouched_tab_lines_come_back_as_loaded() {
        let (shown, tabs) = expand(MERGE_MSG);
        assert_eq!(restore(&shown, &tabs), MERGE_MSG);
    }

    #[test]
    fn edited_lines_stay_as_typed_and_the_others_keep_their_tabs() {
        let (shown, tabs) = expand(MERGE_MSG);
        let edited = shown
            .replace("Merge branch 'dev'", "Merge dev into main")
            .replace("#   src/app.txt", "#   src/app.txt (resolved)");
        assert_eq!(
            restore(&edited, &tabs),
            "Merge dev into main\n\n# Conflicts:\n#\tdocs/guide.md\n#   src/app.txt (resolved)\n"
        );
    }

    #[test]
    fn identical_lines_restore_in_order() {
        let (shown, tabs) = expand("\tx\n\tx\n");
        assert_eq!(shown, "    x\n    x\n");
        assert_eq!(
            restore("    x\n", &tabs),
            "\tx\n",
            "a deleted line takes the first"
        );
        assert_eq!(restore(&shown, &tabs), "\tx\n\tx\n");
    }

    #[test]
    fn multibyte_text_counts_characters() {
        let (shown, _) = expand("é\tx");
        assert_eq!(shown, "é   x");
    }
}
