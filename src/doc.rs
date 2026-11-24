//! Scrollable, lazily rendered text. Only visible rows are ever styled.

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
    /// Kind of every logical line.
    kinds: Vec<Kind>,
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
    pub text: &'a str,
    pub kind: Kind,
}

impl Doc {
    /// Text that came without changes.
    pub fn plain(text: &str) -> Self {
        Doc::new(text, "", None, "")
    }

    /// A summary of the changed files, the code by file, then the talk.
    pub fn new(head: &str, body: &str, diff: Option<&str>, tail: &str) -> Self {
        let mut doc = Doc::default();
        doc.prose(&clean(head));
        let files = diff.map(|d| parse(&clean(d)));
        if let Some(files) = &files {
            doc.summary(files);
        }
        doc.section(body);
        for file in files.iter().flatten() {
            doc.file(file);
        }
        doc.files_end = doc.kinds.len() as u32;
        doc.section(tail);
        doc
    }

    fn section(&mut self, text: &str) {
        if !text.is_empty() {
            self.line("", Kind::Plain);
            self.prose(&clean(text));
        }
    }

    fn line(&mut self, text: &str, kind: Kind) {
        if !self.kinds.is_empty() {
            self.text.push('\n');
        }
        self.text.push_str(text);
        self.kinds.push(kind);
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
            let (mut start, mut used, mut continued) = (offset, 0, false);
            for (i, c) in text.char_indices() {
                let w = c.width().unwrap_or(0);
                if used + w > width as usize {
                    self.rows.push(row(start, offset + i, n, continued));
                    start = offset + i;
                    used = indent(kind, width);
                    continued = true;
                }
                used += w;
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
            let kind = kind.unwrap_or(Kind::Plain);
            Visible {
                indent: if r.continued {
                    indent(kind, self.width)
                } else {
                    0
                },
                text: &self.text[r.start as usize..r.end as usize],
                kind,
            }
        })
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
        let mut doc = Doc::plain("日本語\n\nabc");
        doc.layout(4);
        let rows: Vec<_> = doc.visible(10).map(|r| r.text).collect();
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
        let mut doc = Doc::new("Title", "body", Some(DIFF), "talk");
        doc.layout(60);
        assert_eq!(doc.current_file(), None);
        doc.jump(true, 2);
        assert_eq!(doc.current_file(), Some((1, 3, "src/a b.rs")));
        doc.jump(true, 2);
        assert_eq!(doc.current_file(), Some((2, 3, "new.png")));
        doc.jump(false, 2);
        assert_eq!(doc.current_file(), Some((1, 3, "src/a b.rs")));
    }
}
