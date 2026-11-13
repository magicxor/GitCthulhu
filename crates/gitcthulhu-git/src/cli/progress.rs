//! Parser of Git's `--progress` stderr output (§1.6).
//!
//! Git redraws a progress line with `\r` and ends it with `\n`; `remote:` lines (sideband) are right-padded with
//! spaces before the `\r`. Stderr is therefore split on both `\r` and `\n`, trailing ASCII whitespace is trimmed from
//! every segment, and each segment is matched against this grammar (hand-written, no regex):
//!
//! ```text
//! segment   := ["remote: "] title ":" ws+ ( pct "% (" cur "/" total ")" | cur ) [", " rest] [", done."]
//! title     := [A-Za-z][A-Za-z ()-]*
//! rest      := text up to ", done." or end      e.g. "1.20 MiB | 2.00 MiB/s" -> throughput
//! ```
//!
//! A segment that does not match is a [`Progress::Message`].

use crate::Progress;

/// Parse one trimmed stderr segment. `None` if it is not a progress line (the caller then treats it as a message).
pub(crate) fn parse(segment: &str) -> Option<Progress> {
    let (remote, rest) = match segment.strip_prefix("remote: ") {
        Some(rest) => (true, rest),
        None => (false, segment),
    };

    // title := [A-Za-z][A-Za-z ()-]*, then ':'.
    let colon = rest.find(':')?;
    let title = &rest[..colon];
    let mut chars = title.chars();
    if !chars.next()?.is_ascii_alphabetic()
        || !chars.all(|c| c.is_ascii_alphabetic() || matches!(c, ' ' | '(' | ')' | '-'))
    {
        return None;
    }

    // ws+
    let after_colon = &rest[colon + 1..];
    let body = after_colon.trim_start_matches([' ', '\t']);
    if body.len() == after_colon.len() {
        return None;
    }

    let (body, done) = match body.strip_suffix(", done.") {
        Some(body) => (body, true),
        None => (body, false),
    };

    let (first, after_first) = split_number(body)?;
    let (current, total, percent, tail) = if let Some(after_pct) = after_first.strip_prefix("% (") {
        // pct "% (" cur "/" total ")"
        let percent = u8::try_from(first).ok().filter(|p| *p <= 100)?;
        let (current, after_cur) = split_number(after_pct)?;
        let after_slash = after_cur.strip_prefix('/')?;
        let (total, after_total) = split_number(after_slash)?;
        let tail = after_total.strip_prefix(')')?;
        (current, Some(total), Some(percent), tail)
    } else {
        (first, None, None, after_first)
    };

    let throughput = if tail.is_empty() {
        None
    } else {
        let rest = tail.strip_prefix(", ")?;
        if rest.is_empty() {
            return None;
        }
        Some(rest.to_owned())
    };

    Some(Progress::Phase {
        phase: title.to_owned(),
        remote,
        current,
        total,
        percent,
        throughput,
        done,
    })
}

/// A leading run of ASCII digits as a number, and the text after it.
fn split_number(s: &str) -> Option<(u64, &str)> {
    let end = s.bytes().take_while(u8::is_ascii_digit).count();
    if end == 0 {
        return None;
    }
    Some((s[..end].parse().ok()?, &s[end..]))
}

/// Splits a byte stream into stderr segments: on `\r` and `\n`, trailing ASCII whitespace trimmed, empty segments
/// dropped. Bytes are buffered until a separator arrives, so a segment split across reads stays whole.
#[derive(Default)]
pub(crate) struct SegmentSplitter {
    partial: Vec<u8>,
}

impl SegmentSplitter {
    /// Feed the next chunk; `on_segment` receives every completed segment (lossy UTF-8).
    pub(crate) fn push(&mut self, chunk: &[u8], mut on_segment: impl FnMut(&str)) {
        let mut rest = chunk;
        while let Some(pos) = rest.iter().position(|b| matches!(b, b'\r' | b'\n')) {
            if self.partial.is_empty() {
                emit(&rest[..pos], &mut on_segment);
            } else {
                self.partial.extend_from_slice(&rest[..pos]);
                let partial = std::mem::take(&mut self.partial);
                emit(&partial, &mut on_segment);
            }
            rest = &rest[pos + 1..];
        }
        self.partial.extend_from_slice(rest);
    }

    /// The stream ended: emit what is left.
    pub(crate) fn finish(&mut self, mut on_segment: impl FnMut(&str)) {
        let partial = std::mem::take(&mut self.partial);
        emit(&partial, &mut on_segment);
    }
}

fn emit(segment: &[u8], on_segment: &mut impl FnMut(&str)) {
    let trimmed = segment.trim_ascii_end();
    if trimmed.is_empty() {
        return;
    }
    on_segment(&String::from_utf8_lossy(trimmed));
}

/// Every event a whole stderr byte stream produces: progress phases and messages, in order.
pub(crate) fn events(stderr: &[u8]) -> Vec<Progress> {
    let mut out = Vec::new();
    let mut splitter = SegmentSplitter::default();
    let mut on_segment = |segment: &str| {
        out.push(parse(segment).unwrap_or_else(|| Progress::Message(segment.to_owned())));
    };
    splitter.push(stderr, &mut on_segment);
    splitter.finish(&mut on_segment);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn phase(
        phase: &str,
        remote: bool,
        current: u64,
        total: Option<u64>,
        percent: Option<u8>,
        throughput: Option<&str>,
        done: bool,
    ) -> Option<Progress> {
        Some(Progress::Phase {
            phase: phase.into(),
            remote,
            current,
            total,
            percent,
            throughput: throughput.map(Into::into),
            done,
        })
    }

    #[test]
    fn percentage_with_throughput() {
        assert_eq!(
            parse("Receiving objects:  45% (450/1000), 1.20 MiB | 2.00 MiB/s"),
            phase(
                "Receiving objects",
                false,
                450,
                Some(1000),
                Some(45),
                Some("1.20 MiB | 2.00 MiB/s"),
                false
            ),
            "two spaces after the colon are absorbed"
        );
        assert_eq!(
            parse("Receiving objects: 100% (1000/1000), 2.50 MiB | 2.00 MiB/s, done."),
            phase(
                "Receiving objects",
                false,
                1000,
                Some(1000),
                Some(100),
                Some("2.50 MiB | 2.00 MiB/s"),
                true
            ),
            "throughput stops before `, done.`"
        );
    }

    #[test]
    fn remote_and_count_only_lines() {
        assert_eq!(
            parse("remote: Enumerating objects: 5, done."),
            phase("Enumerating objects", true, 5, None, None, None, true),
            "a count without total"
        );
        assert_eq!(
            parse("remote: Compressing objects:  50% (1/2)"),
            phase(
                "Compressing objects",
                true,
                1,
                Some(2),
                Some(50),
                None,
                false
            ),
            "remote percentage line"
        );
        assert_eq!(
            parse("Receiving objects: 1234, 1.20 MiB | 2.00 MiB/s"),
            phase(
                "Receiving objects",
                false,
                1234,
                None,
                None,
                Some("1.20 MiB | 2.00 MiB/s"),
                false
            ),
            "Git appends throughput to count-only progress, too"
        );
    }

    #[test]
    fn non_progress_lines_are_rejected() {
        for line in [
            "remote: Total 5 (delta 0), reused 0 (delta 0), pack-reused 0",
            "fatal: unable to access 'https://example.invalid/': Could not resolve host",
            "Already up to date.",
            "Rebasing (1/3)",
            "remote:",
            "error: 100% sure",
            "Receiving objects:45% (1/2)",
            "Receiving objects: 145% (1/2)",
            "Receiving objects:  45% (1/2",
            "Receiving objects: 1,",
            "1 thing: 5",
            "remote: Create a pull request for 'x' on GitHub by visiting:",
        ] {
            assert_eq!(parse(line), None, "not a progress line: {line:?}");
        }
    }

    #[test]
    fn splitter_handles_cr_padding_and_chunk_boundaries() {
        let mut segments = Vec::new();
        let mut splitter = SegmentSplitter::default();
        splitter.push(
            b"remote: Counting objects:  50% (1/2)   \rremote: Coun",
            |s| segments.push(s.to_owned()),
        );
        splitter.push(b"ting objects: 100% (2/2), done.   \r\n\r\nfinal", |s| {
            segments.push(s.to_owned())
        });
        splitter.finish(|s| segments.push(s.to_owned()));
        assert_eq!(
            segments,
            [
                "remote: Counting objects:  50% (1/2)",
                "remote: Counting objects: 100% (2/2), done.",
                "final"
            ],
            "padding is trimmed, empty segments dropped, split segments joined"
        );
    }
}
