//! Scrollable, lazily rendered text. Only visible rows are ever styled.

use crate::markdown::{self, Look};
use std::borrow::Cow;
use std::fmt::Write;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Plain,
    Title,
    /// A rendered code block.
    Block,
    Heading,
    /// A file in the "Changed files" summary.
    Summary,
    /// The bar that starts a file's changes.
    File,
    Hunk,
    Add,
    Del,
    Context,
}

impl Kind {
    pub fn is_code(self) -> bool {
        matches!(self, Kind::Add | Kind::Del | Kind::Context)
    }
}

/// Columns of line numbers before the code.
pub const GUTTER: usize = 12;

struct Row {
    start: u32,
    end: u32,
    line: u32,
    continued: bool,
}

#[derive(Default)]
pub struct Doc {
    text: String,
    /// Kind of every logical line, and how far it is indented when wrapped.
    kinds: Vec<Kind>,
    hangs: Vec<u8>,
    /// Byte ranges of `text` with Markdown style bits, in order.
    styles: Vec<(u32, u32, u8)>,
    /// Logical line and path of every changed file.
    files: Vec<(u32, String)>,
    /// First logical line after the code.
    files_end: u32,
    rows: Vec<Row>,
    width: u16,
    pub top: usize,
}

/// One visible row: indent of a wrapped line, its text and kind.
pub struct Visible<'a> {
    pub indent: usize,
    /// Pieces with their Markdown style bits.
    pub parts: Vec<(&'a str, u8)>,
    pub kind: Kind,
}

/// What a document is made of.
#[derive(Debug, Default)]
pub struct Source {
    pub head: String,
    /// Shown before the code, `tail` after it.
    pub body: String,
    pub tail: String,
    pub diff: Option<String>,
    /// `body` and `tail` are Markdown sections, started by `── ` lines.
    pub markdown: bool,
}

impl Source {
    /// Split a preview into its facts and its Markdown.
    pub fn preview(text: &str) -> Self {
        let (head, body) = text.split_once("\n\n").unwrap_or((text, ""));
        Source {
            head: head.to_string(),
            body: body.to_string(),
            markdown: true,
            ..Source::default()
        }
    }

    pub fn plain(text: String) -> Self {
        Source {
            head: text,
            ..Source::default()
        }
    }
}

impl Doc {
    /// Facts, a summary of the changed files, the code by file, then the talk.
    pub fn new(source: &Source, raw: bool) -> Self {
        let mut doc = Doc::default();
        let rendered = source.markdown && !raw;
        doc.prose(&clean(&source.head));
        let files = source.diff.as_deref().map(|d| parse(&clean(d)));
        if let Some(files) = &files {
            doc.summary(files);
        }
        doc.section(&source.body, rendered);
        for file in files.iter().flatten() {
            doc.file(file);
        }
        doc.files_end = doc.kinds.len() as u32;
        doc.section(&source.tail, rendered);
        doc
    }

    fn section(&mut self, text: &str, rendered: bool) {
        if text.is_empty() {
            return;
        }
        self.line("", Kind::Plain);
        let text = clean(text);
        if !rendered {
            return self.prose(&text);
        }
        // Rendered one by one, so that an unfinished block ends with its section.
        let mut rest: &str = &text;
        while !rest.is_empty() {
            let (title, after) = match rest.starts_with("── ") {
                true => rest.split_once('\n').unwrap_or((rest, "")),
                false => ("", rest),
            };
            let next = match after.starts_with("── ") {
                true => 0,
                false => after.find("\n── ").map_or(after.len(), |i| i + 1),
            };
            if !title.is_empty() {
                self.line(title, Kind::Heading);
            }
            self.markdown(&after[..next]);
            rest = &after[next..];
            if !rest.is_empty() {
                self.line("", Kind::Plain);
            }
        }
    }

    fn markdown(&mut self, source: &str) {
        for line in markdown::render(source) {
            // Entities such as `&#27;` may have brought control characters back.
            let safe = clean(&line.text);
            let spans = if safe.len() == line.text.len() {
                line.spans
            } else {
                Vec::new()
            };
            let kind = match line.look {
                Look::Text => Kind::Plain,
                Look::Heading => Kind::Heading,
                Look::Code => Kind::Block,
            };
            self.line(&safe.replace('\n', " "), kind);
            let start = self.text.len() - safe.len();
            let at = |i: usize| (start + i) as u32;
            let styles = spans.into_iter().map(|(a, b, s)| (at(a), at(b), s));
            self.styles.extend(styles);
            if let Some(hang) = self.hangs.last_mut() {
                *hang = line.hang;
            }
        }
    }

    fn line(&mut self, text: &str, kind: Kind) {
        if !self.kinds.is_empty() {
            self.text.push('\n');
        }
        self.text.push_str(text);
        self.kinds.push(kind);
        self.hangs.push(0);
    }

    fn prose(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        for line in text.split('\n') {
            let kind = if line.starts_with("── ")
                || ["# ", "## ", "### "].iter().any(|h| line.starts_with(h))
            {
                Kind::Heading
            } else if self.kinds.is_empty() {
                Kind::Title
            } else {
                Kind::Plain
            };
            self.line(line, kind);
        }
    }

    fn summary(&mut self, files: &[File]) {
        let (add, del) = files
            .iter()
            .fold((0, 0), |(a, d), f| (a + f.added, d + f.deleted));
        if !self.kinds.is_empty() {
            self.line("", Kind::Plain);
        }
        self.line(
            &format!("── Changed files ({})  +{add} -{del}", files.len()),
            Kind::Heading,
        );
        for f in files {
            let text = format!(
                " {} {:>6} {:>6}  {}",
                f.status,
                format!("+{}", f.added),
                format!("-{}", f.deleted),
                f.path
            );
            self.line(&text, Kind::Summary);
        }
    }

    fn file(&mut self, file: &File) {
        self.line("", Kind::Plain);
        self.files
            .push((self.kinds.len() as u32, file.path.clone()));
        let name = match file.status {
            'A' => "added",
            'D' => "deleted",
            'R' => "renamed",
            _ => "modified",
        };
        let title = format!(
            " ▌ {}  ({name})  +{} -{}",
            file.path, file.added, file.deleted
        );
        self.line(&title, Kind::File);
        for (kind, text) in &file.lines {
            self.line(text, *kind);
        }
    }

    pub fn has_files(&self) -> bool {
        !self.files.is_empty()
    }

    /// Position, count and path of the file at the top of the screen.
    pub fn current_file(&self) -> Option<(usize, usize, &str)> {
        let line = self.rows.get(self.top)?.line;
        if line >= self.files_end {
            return None;
        }
        let n = self.files.partition_point(|f| f.0 <= line);
        let file = self.files.get(n.checked_sub(1)?)?;
        Some((n, self.files.len(), &file.1))
    }

    /// Scroll to the next (or previous) file.
    pub fn jump(&mut self, forward: bool, height: usize) {
        let rows = &self.rows;
        let row_of = |line: u32| rows.partition_point(|r| r.line < line);
        let starts = self.files.iter().map(|f| row_of(f.0));
        let to = if forward {
            starts.clone().find(|&r| r > self.top)
        } else {
            starts.rev().find(|&r| r < self.top)
        };
        if let Some(row) = to {
            self.top = row;
            self.scroll(0, height);
        }
    }

    pub fn layout(&mut self, width: u16) {
        let width = width.max(2);
        if self.width == width {
            return;
        }
        let line = self.rows.get(self.top).map(|r| r.line);
        self.width = width;
        self.rows.clear();
        let mut offset = 0;
        for (n, text) in self.text.split('\n').enumerate() {
            let kind = self.kinds.get(n).copied().unwrap_or(Kind::Plain);
            let hang = self.hang(n, width);
            let words = !kind.is_code() && kind != Kind::Block;
            let (mut start, mut used, mut continued) = (offset, 0, false);
            // Where the last word ended, and the columns up to there.
            let mut space: Option<(usize, usize)> = None;
            for (i, c) in text.char_indices() {
                let w = c.width().unwrap_or(0);
                if used + w > width as usize {
                    let (end, kept) = match space {
                        Some((end, columns)) if words && c != ' ' => (end, used - columns),
                        _ => (offset + i, 0),
                    };
                    self.rows.push(row(start, end, n, continued));
                    start = end;
                    used = hang + kept;
                    continued = true;
                    space = None;
                }
                used += w;
                if c == ' ' && !text[..i].trim_start().is_empty() {
                    space = Some((offset + i + 1, used));
                }
            }
            self.rows
                .push(row(start, offset + text.len(), n, continued));
            offset += text.len() + 1;
        }
        if let Some(line) = line {
            // Keep the same line on top after a resize.
            self.top = self.rows.partition_point(|r| r.line < line);
        }
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn visible(&self, height: usize) -> impl Iterator<Item = Visible<'_>> {
        self.rows.iter().skip(self.top).take(height).map(|r| {
            let kind = self.kinds.get(r.line as usize).copied();
            Visible {
                indent: if r.continued {
                    self.hang(r.line as usize, self.width)
                } else {
                    0
                },
                parts: self.parts(r.start, r.end),
                kind: kind.unwrap_or(Kind::Plain),
            }
        })
    }

    /// Cut a row at its style changes.
    fn parts(&self, start: u32, end: u32) -> Vec<(&str, u8)> {
        let text = |a: u32, b: u32| &self.text[a as usize..b as usize];
        let mut parts = Vec::new();
        let mut at = start;
        let first = self.styles.partition_point(|s| s.1 <= start);
        for &(a, b, style) in self.styles[first..].iter().take_while(|s| s.0 < end) {
            let (a, b) = (a.max(at), b.min(end));
            if a > at {
                parts.push((text(at, a), 0));
            }
            if b > a {
                parts.push((text(a, b), style));
                at = b;
            }
        }
        if at < end || parts.is_empty() {
            parts.push((text(at, end), 0));
        }
        parts
    }

    /// Indent of a wrapped line; none when there is little room.
    fn hang(&self, line: usize, width: u16) -> usize {
        let kind = self.kinds.get(line).copied().unwrap_or(Kind::Plain);
        let hang = self.hangs.get(line).copied().unwrap_or(0) as usize;
        match indent(kind, width) {
            0 if hang * 2 < width as usize => hang,
            n => n,
        }
    }

    /// Show the `n`-th file (from 1) at the top.
    pub fn show_file(&mut self, n: usize, height: usize) {
        if let Some((line, _)) = self.files.get(n.saturating_sub(1)) {
            self.top = self.rows.partition_point(|r| r.line < *line);
            self.scroll(0, height);
        }
    }

    pub fn width(&self) -> u16 {
        self.width
    }

    pub fn scroll(&mut self, by: isize, height: usize) {
        let max = self.rows.len().saturating_sub(height);
        self.top = self.top.saturating_add_signed(by).min(max);
    }
}

/// Wrapped code stays right of the line numbers when there is room.
fn indent(kind: Kind, width: u16) -> usize {
    if kind.is_code() && width as usize > GUTTER * 2 {
        GUTTER + 2
    } else {
        0
    }
}

fn row(start: usize, end: usize, line: usize, continued: bool) -> Row {
    // Documents are capped at a few MiB, far below u32::MAX.
    Row {
        start: start as u32,
        end: end as u32,
        line: line as u32,
        continued,
    }
}

#[derive(Debug, PartialEq)]
struct File {
    path: String,
    status: char,
    added: usize,
    deleted: usize,
    lines: Vec<(Kind, String)>,
}

/// `@@ -1,2 +3,4 @@` -> (old start, old count, new start, new count)
fn hunk(line: &str) -> Option<[usize; 4]> {
    let rest = line.strip_prefix("@@ -")?;
    let (old, rest) = rest.split_once(" +")?;
    let (new, _) = rest.split_once(" @@")?;
    let range = |r: &str| -> Option<(usize, usize)> {
        match r.split_once(',') {
            Some((s, n)) => Some((s.parse().ok()?, n.parse().ok()?)),
            None => Some((r.parse().ok()?, 1)),
        }
    };
    let ((a, b), (c, d)) = (range(old)?, range(new)?);
    Some([a, b, c, d])
}

/// Split a git diff into files, numbering every code line.
fn parse(diff: &str) -> Vec<File> {
    let mut files: Vec<File> = Vec::new();
    // Remaining lines of the current hunk: content such as `--- x` inside a
    // hunk must not be mistaken for a header.
    let (mut old, mut old_left, mut new, mut new_left) = (0usize, 0usize, 0usize, 0usize);
    for line in diff.lines() {
        if old_left == 0
            && new_left == 0
            && let Some(names) = line.strip_prefix("diff --git ")
        {
            let path = names.rsplit_once(" b/").map_or(names, |n| n.1);
            files.push(File {
                path: path.trim_matches('"').to_string(),
                status: 'M',
                added: 0,
                deleted: 0,
                lines: Vec::new(),
            });
            continue;
        }
        let Some(file) = files.last_mut() else {
            continue;
        };
        if old_left > 0 || new_left > 0 {
            let mut text = String::with_capacity(line.len() + GUTTER + 3);
            let code = line.get(1..).unwrap_or("");
            // Writing to a String cannot fail.
            let (kind, result) = match line.as_bytes().first() {
                Some(b'+') if new_left > 0 => {
                    (Kind::Add, write!(text, "{:>5} {new:>5} │+{code}", ""))
                }
                Some(b'-') if old_left > 0 => {
                    (Kind::Del, write!(text, "{old:>5} {:>5} │-{code}", ""))
                }
                Some(b'\\') => (Kind::Context, write!(text, "{:>11} │{line}", "")),
                _ => (Kind::Context, write!(text, "{old:>5} {new:>5} │ {code}")),
            };
            debug_assert!(result.is_ok());
            if !line.starts_with('\\') {
                if kind != Kind::Add {
                    old += 1;
                    old_left = old_left.saturating_sub(1);
                }
                if kind != Kind::Del {
                    new += 1;
                    new_left = new_left.saturating_sub(1);
                }
            }
            file.added += (kind == Kind::Add) as usize;
            file.deleted += (kind == Kind::Del) as usize;
            file.lines.push((kind, text));
        } else if let Some([a, b, c, d]) = hunk(line) {
            (old, old_left, new, new_left) = (a, b, c, d);
            file.lines.push((Kind::Hunk, line.to_string()));
        } else if line.starts_with("new file") {
            file.status = 'A';
        } else if line.starts_with("deleted file") {
            file.status = 'D';
        } else if let Some(from) = line.strip_prefix("rename from ") {
            file.status = 'R';
            file.lines
                .push((Kind::Hunk, format!("renamed from {from}")));
        } else if let Some(to) = line.strip_prefix("+++ b/") {
            file.path = to.trim_end().to_string();
        } else if line.starts_with("Binary files") || line.starts_with('…') {
            file.lines.push((Kind::Hunk, line.to_string()));
        }
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(row: &Visible) -> String {
        " ".repeat(row.indent) + &row.parts.iter().map(|p| p.0).collect::<String>()
    }

    const DIFF: &str = "diff --git a/src/a b.rs b/src/a b.rs
index 1..2 100644
--- a/src/a b.rs
+++ b/src/a b.rs
@@ -1,3 +1,3 @@ fn x
 keep
--- not a header
+++ not a header either
 end
\\ No newline at end of file
diff --git a/old.png b/new.png
similarity index 90%
rename from old.png
rename to new.png
Binary files a/old.png and b/new.png differ
diff --git a/gone b/gone
deleted file mode 100644
--- a/gone
+++ /dev/null
@@ -1 +0,0 @@
-bye
";

    #[test]
    fn strips_terminal_escapes() {
        assert_eq!(clean("a\u{1b}[31mb\u{1b}[0m\r\n\tc"), "ab\n    c");
        assert_eq!(clean("x\u{1b}]0;t\u{7}\u{202e}y"), "x]0;ty");
        assert!(matches!(clean("plain\ntext"), Cow::Borrowed(_)));
    }

    #[test]
    fn wraps_wide_text_and_clamps_scroll() {
        let mut doc = Doc::new(&Source::plain("日本語\n\nabc".into()), false);
        doc.layout(4);
        let rows: Vec<_> = doc.visible(10).map(|r| text(&r)).collect();
        assert_eq!(rows, ["日本", "語", "", "abc"]);
        doc.scroll(99, 3);
        assert_eq!(doc.top, 1);
        doc.scroll(-99, 3);
        assert_eq!(doc.top, 0);
    }

    #[test]
    fn groups_changes_by_file() {
        let files = parse(DIFF);
        let got: Vec<_> = files
            .iter()
            .map(|f| (f.path.as_str(), f.status, f.added, f.deleted))
            .collect();
        assert_eq!(
            got,
            [
                ("src/a b.rs", 'M', 1, 1),
                ("new.png", 'R', 0, 0),
                ("gone", 'D', 0, 1)
            ]
        );
        assert_eq!(files[0].lines[1].1, "    1     1 │ keep");
        assert_eq!(
            files[0].lines[2],
            (Kind::Del, "    2       │--- not a header".into())
        );
        assert_eq!(
            files[0].lines[3],
            (Kind::Add, "          2 │+++ not a header either".into())
        );
        assert_eq!(files[0].lines[4].1, "    3     3 │ end");
    }

    #[test]
    fn jumps_between_files() {
        let source = Source {
            head: "Title".into(),
            body: "body".into(),
            tail: "talk".into(),
            diff: Some(DIFF.into()),
            markdown: true,
        };
        let mut doc = Doc::new(&source, false);
        doc.layout(60);
        assert_eq!(doc.current_file(), None);
        doc.jump(true, 2);
        assert_eq!(doc.current_file(), Some((1, 3, "src/a b.rs")));
        doc.jump(true, 2);
        assert_eq!(doc.current_file(), Some((2, 3, "new.png")));
        doc.jump(false, 2);
        assert_eq!(doc.current_file(), Some((1, 3, "src/a b.rs")));
    }

    fn rows(doc: &mut Doc, width: u16) -> Vec<String> {
        doc.layout(width);
        doc.visible(99).map(|r| text(&r)).collect()
    }

    #[test]
    fn wraps_at_words_with_hanging_indent() {
        let source = Source::preview("T\n\n- one two three four");
        let mut doc = Doc::new(&source, false);
        assert_eq!(rows(&mut doc, 12), ["T", "", "• one two ", "  three four"]);
        let mut doc = Doc::new(&Source::plain("abcdefghij kl".into()), false);
        assert_eq!(rows(&mut doc, 4), ["abcd", "efgh", "ij ", "kl"]);
    }

    #[test]
    fn sections_do_not_leak_and_raw_is_untouched() {
        let body = "── Description\n**b**\n```\nopen\n\n── @al commented\n*hi*";
        let source = Source {
            head: "T".into(),
            body: body.into(),
            markdown: true,
            ..Source::default()
        };
        let got = rows(&mut Doc::new(&source, false), 60);
        assert_eq!(
            got,
            [
                "T",
                "",
                "── Description",
                "b",
                "",
                "  open",
                "",
                "── @al commented",
                "hi"
            ]
        );
        let got = rows(&mut Doc::new(&source, true), 60);
        assert_eq!(got.join("\n"), format!("T\n\n{body}"));
    }

    #[test]
    fn entities_cannot_bring_escapes_back() {
        let source = Source::preview("T\n\n**a**&#27;[31m&#x202e;b");
        let got = rows(&mut Doc::new(&source, false), 60);
        assert!(!got.concat().contains(['\u{1b}', '\u{202e}']), "{got:?}");
    }

    #[test]
    fn styles_follow_wrapped_rows() {
        let mut doc = Doc::new(&Source::preview("T\n\naa **bb cc** dd"), false);
        doc.layout(6);
        let parts: Vec<_> = doc.visible(9).skip(2).map(|r| r.parts).collect();
        assert_eq!(
            parts,
            [
                vec![("aa ", 0), ("bb ", markdown::BOLD)],
                vec![("cc", markdown::BOLD), (" dd", 0)]
            ]
        );
    }
}
