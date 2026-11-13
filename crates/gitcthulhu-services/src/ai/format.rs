//! Cleaning and shaping of what a model answers: reasoning blocks, code fences, the 72-column commit
//! message layout, pull-request title/body and branch-name sanitizing.
//!
//! Ported from the GitButler GUI (`but-gpui/src/backend/ai.rs`) and `but-action/src/commit_format.rs`, with their
//! tests. Every function here is pure and safe to call on a partial answer while it streams in.

use gix::bstr::ByteSlice as _;

/// `text` without the reasoning a reasoning model (DeepSeek-R1 or Qwen3 run locally, say) can put before its
/// answer in `<think>` tags, and nothing while it is still reasoning.
pub fn without_thinking(text: &str) -> &str {
    match text.trim_start().strip_prefix("<think>") {
        // Only blank lines go, as a suggested merge's first line keeps its indentation.
        Some(thinking) => (thinking.split_once("</think>"))
            .map_or("", |(_, answer)| answer.trim_start_matches(['\n', '\r'])),
        None => text,
    }
}

/// `text` without a code fence around it, also while it streams in. Only blank lines go around it, as code needs
/// its first line's indentation.
pub fn strip_fence(text: &str) -> &str {
    fn lines(text: &str) -> &str {
        text.trim_start_matches(['\n', '\r']).trim_end()
    }
    let text = lines(text);
    let Some(after) = text.trim_start().strip_prefix("```") else {
        return text;
    };
    // Past the opener's language tag; a lone opener mid-stream goes too.
    let Some((_, inner)) = after.split_once('\n') else {
        return "";
    };
    match inner.trim_end().strip_suffix("```") {
        Some(inner) if !inner.contains("```") => lines(inner),
        Some(_) => text,
        None if inner.contains("```") => text,
        None => lines(inner),
    }
}

/// Split a generated pull request into its title and body, dropping a fence around the whole answer and a first
/// body line that only repeats the title. Safe on a partial answer, as it streams.
pub fn split_title_body(text: &str) -> (String, String) {
    let text = strip_fence(text);
    let clean = |line: &str| {
        line.trim()
            .trim_start_matches('#')
            .trim()
            .trim_matches(|c| matches!(c, '"' | '\'' | '`'))
            .trim()
            .to_owned()
    };
    let Some((title, rest)) = text.split_once('\n') else {
        return (clean(text), String::new());
    };
    let title = clean(title);
    let rest = rest.trim();
    let (first, after) = rest.split_once('\n').unwrap_or((rest, ""));
    let body = if !title.is_empty() && clean(first).eq_ignore_ascii_case(&title) {
        after.trim()
    } else {
        rest
    };
    (title, body.to_owned())
}

/// A branch name from a model's answer, or `None` if nothing usable is in it.
///
/// Takes the first line (thinking and fences removed), strips quotes and backticks, lower-cases it, joins words
/// with `-` and trims separators from the ends. The result must be a valid branch name by Git's rules
/// (`git check-ref-format --branch`); it is not checked against existing branches.
pub fn branch_name_from_answer(answer: &str) -> Option<String> {
    let answer = strip_fence(without_thinking(answer));
    let line = answer
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    let line = line.trim_matches(|c| matches!(c, '"' | '\'' | '`')).trim();
    let joined = line
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-")
        .to_lowercase();
    let name = joined.trim_matches(['-', '/', '.']);
    if name.is_empty() || name == "head" || name == "@" {
        return None;
    }
    let full = format!("refs/heads/{name}");
    gix::validate::reference::name(full.as_bytes().as_bstr()).ok()?;
    Some(name.to_owned())
}

/// Format a commit message to follow the email RFC layout with 72-column wrapping.
///
/// - the subject (first line) is kept as is;
/// - body paragraphs are wrapped at 72 characters (counted in characters, not bytes) at word boundaries;
/// - blank lines, list items (`-`, `*`, `#`, `>`, numbered `1.`/`2)`), 4-space-indented lines and code between
///   fences are kept as written;
/// - a final paragraph in which every line is a `Key: value` (trailers such as `Signed-off-by:`) is kept line by
///   line.
pub fn format_commit_message(message: &str) -> String {
    let lines: Vec<&str> = message.lines().collect();

    if lines.is_empty() {
        return message.to_string();
    }

    let mut result = Vec::new();

    // First line (subject) is preserved as-is
    result.push(lines[0].to_string());

    let trailers = trailers_start(&lines);
    let mut fenced = false;
    // Process the rest of the message
    let mut i = 1;
    while i < lines.len() {
        let line = lines[i];

        // Code between fences and the trailers stay line by line, as written
        let fence = line.trim_start().starts_with("```");
        let kept = fenced || fence || i >= trailers;
        fenced ^= fence;
        if kept {
            result.push(line.to_string());
            i += 1;
            continue;
        }

        // Preserve blank lines
        if line.trim().is_empty() {
            result.push(String::new());
            i += 1;
            continue;
        }

        // Detect list items or special formatting (lines starting with -, *, #, etc.)
        if is_special_format(line) {
            result.push(line.to_string());
            i += 1;
            continue;
        }

        // Otherwise, collect lines into a paragraph and wrap
        let mut paragraph = line.to_string();
        i += 1;

        // Collect continuation lines (non-blank, non-special lines)
        while i < trailers && !lines[i].trim().is_empty() && !is_special_format(lines[i]) {
            paragraph.push(' ');
            paragraph.push_str(lines[i].trim());
            i += 1;
        }

        // Wrap the paragraph to 72 characters
        result.extend(wrap_paragraph(&paragraph, 72));
    }

    result.join("\n")
}

/// Checks if a line has special formatting that should be preserved
fn is_special_format(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed.starts_with('-')
        || trimmed.starts_with('*')
        || trimmed.starts_with('#')
        || trimmed.starts_with('>')
        || trimmed.starts_with("```")
        || line.starts_with("    ") // Indented code blocks
        || is_numbered_item(trimmed)
}

/// Whether `line` starts a numbered list item, like `1. ` or `2) `.
fn is_numbered_item(line: &str) -> bool {
    line.split_once(['.', ')']).is_some_and(|(n, rest)| {
        !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()) && rest.starts_with(' ')
    })
}

/// Where the trailers start, as `Signed-off-by:` and `Co-authored-by:`: the last paragraph when every line of it
/// is a `Key: value`, as Git reads them; otherwise past the end.
fn trailers_start(lines: &[&str]) -> usize {
    let end = lines
        .iter()
        .rposition(|line| !line.trim().is_empty())
        .map_or(0, |last| last + 1);
    let Some(blank) = lines[..end].iter().rposition(|line| line.trim().is_empty()) else {
        return lines.len();
    };
    let is_trailer = |line: &&str| {
        line.split_once(": ").is_some_and(|(key, _)| {
            !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
    };
    match lines[blank + 1..end].iter().all(is_trailer) {
        true => blank + 1,
        false => lines.len(),
    }
}

/// Wraps a paragraph to the specified width, breaking at word boundaries
fn wrap_paragraph(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current_line = String::new();
    // In characters, as they're read, not bytes
    let mut current_width = 0;

    for word in text.split_whitespace() {
        let word_width = word.chars().count();
        // If adding this word would exceed the width
        if !current_line.is_empty() && current_width + 1 + word_width > width {
            lines.push(current_line);
            current_line = word.to_string();
            current_width = word_width;
        } else {
            if !current_line.is_empty() {
                current_line.push(' ');
                current_width += 1;
            }
            current_line.push_str(word);
            current_width += word_width;
        }
    }

    // Add the last line if there's anything left
    if !current_line.is_empty() {
        lines.push(current_line);
    }

    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reasoning_models_thinking_is_left_out() {
        let answer = "<think>\nThe diff adds pagination.\n</think>\n\nPaginate orders";
        assert_eq!(
            without_thinking(answer),
            "Paginate orders",
            "only the answer goes in"
        );
        assert_eq!(
            without_thinking("<think>\nThe diff"),
            "",
            "nothing shows while it reasons"
        );
        assert_eq!(
            without_thinking("<think>merge</think>\n\n    return a"),
            "    return a",
            "code after it keeps its first line's indentation"
        );
        assert_eq!(
            without_thinking("Mention <think> tags"),
            "Mention <think> tags",
            "an answer that only names the tag stays"
        );
    }

    #[test]
    fn strip_fence_keeps_the_first_lines_indentation() {
        let code = "    return a\n}";
        assert_eq!(strip_fence(code), code, "unfenced, only blank lines go");
        assert_eq!(
            strip_fence("\n```python\n    return a\n}\n```\n"),
            code,
            "fenced, the fence and blank lines go, the indentation stays"
        );
    }

    #[test]
    fn strip_fence_handles_partial_streams() {
        assert_eq!(strip_fence("```"), "", "a lone opener shows nothing yet");
        assert_eq!(
            strip_fence("```text\nfeat: add"),
            "feat: add",
            "no closer yet"
        );
        assert_eq!(
            strip_fence("```text\nfeat: add\n```"),
            "feat: add",
            "closed fence"
        );
        let two = "```a\none\n```\ntext\n```b\ntwo\n```";
        assert_eq!(
            strip_fence(two),
            two,
            "fences that are not around the whole answer stay"
        );
    }

    #[test]
    fn splits_title_from_body_and_drops_an_echoed_heading() {
        let (title, body) =
            split_title_body("```markdown\n# Add search\n\n# Add search\n- Fast\n```");
        assert_eq!(title, "Add search", "fence and heading marks go");
        assert_eq!(body, "- Fast", "the body doesn't repeat the title");
        let (title, body) = split_title_body("Add search");
        assert_eq!(
            (title.as_str(), body.as_str()),
            ("Add search", ""),
            "a partial answer is all title"
        );
    }

    #[test]
    fn branch_names_are_cleaned_and_validated() {
        for (answer, expected) in [
            ("feat/add-login", Some("feat/add-login")),
            ("`Fix Login Redirect`\n", Some("fix-login-redirect")),
            ("```\n\"feat/Add search\"\n```", Some("feat/add-search")),
            (
                "<think>hmm</think>\nchore/bump-deps\nbecause...",
                Some("chore/bump-deps"),
            ),
            ("  \n\n", None),
            ("<think>still thinking", None),
            ("feat/a..b", None),
            ("HEAD", None),
            ("@", None),
            ("x~1", None),
        ] {
            assert_eq!(
                branch_name_from_answer(answer).as_deref(),
                expected,
                "{answer:?}"
            );
        }
    }

    #[test]
    fn test_format_simple_message() {
        let input = "Add user authentication\n\nThis commit adds a basic user authentication system to the application. It includes login and logout functionality with JWT tokens.";
        let output = format_commit_message(input);

        // Subject should be preserved
        assert!(output.starts_with("Add user authentication\n\n"));

        // Body lines should not exceed 80 characters
        for line in output.lines().skip(2) {
            if !line.is_empty() {
                assert!(line.len() <= 80, "Line too long: {line}");
            }
        }
    }

    #[test]
    fn test_preserve_list_items() {
        let input = "Fix multiple bugs\n\n- Fix authentication timeout\n- Fix database connection pool\n- Fix UI rendering issue";
        let output = format_commit_message(input);

        assert!(output.contains("- Fix authentication timeout"));
        assert!(output.contains("- Fix database connection pool"));
        assert!(output.contains("- Fix UI rendering issue"));
    }

    #[test]
    fn test_wrap_long_paragraph() {
        let input = "Add feature\n\nThis is a very long paragraph that definitely exceeds 72 characters and should be wrapped into multiple lines when formatted properly according to email RFC standards.";
        let output = format_commit_message(input);

        // Check that body lines don't exceed 72 characters
        for (i, line) in output.lines().enumerate() {
            if i > 1 && !line.is_empty() {
                assert!(
                    line.len() <= 72,
                    "Line {} too long: '{}' ({})",
                    i,
                    line,
                    line.len()
                );
            }
        }
    }

    #[test]
    fn test_preserve_blank_lines() {
        let input = "Add feature\n\nFirst paragraph.\n\nSecond paragraph.";
        let output = format_commit_message(input);

        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(lines[0], "Add feature");
        assert_eq!(lines[1], "");
        assert!(lines[2].starts_with("First"));
        assert_eq!(lines[3], "");
        assert!(lines[4].starts_with("Second"));
    }

    #[test]
    fn trailers_numbered_lists_and_code_stay_as_written() {
        let long = "word ".repeat(20);
        let input = format!(
            "Subject\n\n{long}\n\n1. first\n2. second\n\n```\nlet a = 1;\nlet b = 2;\n```\n\nSigned-off-by: A <a@x.io>\nCo-authored-by: B <b@x.io>"
        );
        let output = format_commit_message(&input);
        for kept in [
            "\n1. first\n2. second\n",
            "\n```\nlet a = 1;\nlet b = 2;\n```\n",
            "\nSigned-off-by: A <a@x.io>\nCo-authored-by: B <b@x.io>",
        ] {
            assert!(
                output.contains(kept),
                "{kept:?} is kept line by line in {output:?}"
            );
        }
    }

    #[test]
    fn width_counts_characters_not_bytes() {
        let input = format!("Subject\n\n{}", "한국어 ".repeat(20));
        let output = format_commit_message(&input);
        let widest = (output.lines().skip(2))
            .map(|line| line.chars().count())
            .max();
        assert!(
            widest.is_some_and(|w| w > 60),
            "lines of three-byte characters fill the width too: {output:?}"
        );
    }
}
