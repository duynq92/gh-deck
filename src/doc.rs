//! Scrollable, lazily rendered text. Only visible rows are ever styled.

use std::borrow::Cow;
use unicode_width::UnicodeWidthChar;

/// Remove ANSI escapes, control and bidi-override characters, because text from
/// GitHub is untrusted and must not be able to drive the terminal.
pub fn clean(text: &str) -> Cow<'_, str> {
    let bad = |c: char| {
        (c.is_control() && c != '\n')
            || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{feff}')
    };
    if !text.contains(bad) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\t' => out.push_str("    "),
            '\u{1b}' if chars.peek() == Some(&'[') => {
                chars.next();
                while let Some(&c) = chars.peek() {
                    if c == '\n' {
                        break;
                    }
                    chars.next();
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            c if bad(c) => {}
            c => out.push(c),
        }
    }
    Cow::Owned(out)
}

struct Row {
    start: u32,
    end: u32,
    line: u32,
}

#[derive(Default)]
pub struct Doc {
    text: String,
    rows: Vec<Row>,
    width: u16,
    pub top: usize,
    pub diff: bool,
}

impl Doc {
    pub fn new(text: &str, diff: bool) -> Self {
        Doc {
            text: clean(text).into_owned(),
            diff,
            ..Doc::default()
        }
    }

    pub fn layout(&mut self, width: u16) {
        let width = width.max(2);
        if self.width == width {
            return;
        }
        self.width = width;
        self.rows.clear();
        let mut line = 0;
        for text in self.text.split('\n') {
            let (mut start, mut used) = (line, 0);
            for (i, c) in text.char_indices() {
                let w = c.width().unwrap_or(0);
                if used + w > width as usize {
                    self.rows.push(row(start, line + i, line));
                    start = line + i;
                    used = 0;
                }
                used += w;
            }
            self.rows.push(row(start, line + text.len(), line));
            line += text.len() + 1;
        }
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Visible rows, each with the logical line it belongs to.
    pub fn visible(&self, height: usize) -> impl Iterator<Item = (&str, &str)> {
        self.rows.iter().skip(self.top).take(height).map(|r| {
            let line = &self.text[r.line as usize..];
            let first = line.split('\n').next().unwrap_or("");
            (&self.text[r.start as usize..r.end as usize], first)
        })
    }

    pub fn scroll(&mut self, by: isize, height: usize) {
        let max = self.rows.len().saturating_sub(height);
        self.top = self.top.saturating_add_signed(by).min(max);
    }
}

fn row(start: usize, end: usize, line: usize) -> Row {
    // Documents are capped at a few MiB, far below u32::MAX.
    Row {
        start: start as u32,
        end: end as u32,
        line: line as u32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_terminal_escapes() {
        assert_eq!(clean("a\u{1b}[31mb\u{1b}[0m\r\n\tc"), "ab\n    c");
        assert_eq!(clean("x\u{1b}]0;t\u{7}\u{202e}y"), "x]0;ty");
        assert!(matches!(clean("plain\ntext"), Cow::Borrowed(_)));
    }

    #[test]
    fn wraps_wide_text_and_clamps_scroll() {
        let mut doc = Doc::new("日本語\n\nabc", false);
        doc.layout(4);
        let rows: Vec<_> = doc.visible(10).map(|r| r.0).collect();
        assert_eq!(rows, ["日本", "語", "", "abc"]);
        doc.scroll(99, 3);
        assert_eq!(doc.top, 1);
        doc.scroll(-99, 3);
        assert_eq!(doc.top, 0);
    }
}
