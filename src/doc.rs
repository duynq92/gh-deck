//! Scrollable, lazily rendered text. Only visible rows are ever styled.

use crate::ai::{Finding, Review};
use crate::markdown::{self, Look};
use crate::review::{Place, Thread};
use std::borrow::Cow;
use std::fmt::Write;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

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
    /// First line of an unresolved or resolved discussion.
    Open,
    Done,
    /// First line of an AI finding, new or accepted.
    Finding,
    Accepted,
    /// The rest of a discussion.
    Note,
}

impl Kind {
    /// Lines the cursor stops at.
    fn is_target(self) -> bool {
        self.is_code()
            || matches!(
                self,
                Kind::Open | Kind::Done | Kind::Finding | Kind::Accepted
            )
    }

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
    /// Logical line and position of every discussion.
    threads: Vec<(u32, usize)>,
    findings: Vec<(u32, usize)>,
    /// The line that sums the findings up: not a stop.
    findings_line: Option<u32>,
    /// Line mode: the current line, and where a range started.
    pub cursor: Option<u32>,
    pub anchor: Option<u32>,
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
    /// Under the cursor, or in its range.
    pub selected: bool,
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
    pub threads: Vec<Thread>,
    /// Show resolved discussions in full.
    pub show_resolved: bool,
    /// Commit the diff belongs to.
    pub oid: String,
    pub ai: Option<Review>,
}

#[derive(Debug, PartialEq)]
pub enum Mark {
    Thread(usize),
    Finding(usize),
    Line(String, Option<u32>, Option<u32>),
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
        if let Some(ai) = &source.ai {
            doc.line(&clean(&ai.summary()).replace('\n', " "), Kind::Finding);
            doc.findings_line = Some(doc.kinds.len() as u32 - 1);
        }
        let files = source.diff.as_deref().map(|d| parse(&clean(d)));
        if let Some(files) = &files {
            doc.summary(files);
        }
        doc.section(&source.body, rendered);
        for file in files.iter().flatten() {
            doc.file(file, source, rendered);
        }
        let known = |t: &Thread| files.iter().flatten().any(|f| f.path == t.path);
        let mut first = true;
        for (n, thread) in source.threads.iter().enumerate() {
            if known(thread) {
                continue;
            }
            if std::mem::take(&mut first) {
                doc.line("", Kind::Plain);
                doc.line("── Discussions on other files", Kind::Heading);
            }
            doc.thread(n, thread, source, rendered);
        }
        let found = source.ai.iter().flat_map(|r| &r.findings).enumerate();
        let known = |f: &Finding| files.iter().flatten().any(|c| c.path == f.path);
        for (n, finding) in found.filter(|(_, f)| !known(f)) {
            if std::mem::take(&mut first) {
                doc.line("", Kind::Plain);
                doc.line("── Discussions on other files", Kind::Heading);
            }
            doc.finding(n, finding, rendered);
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
            self.markdown(&after[..next], "", None);
            rest = &after[next..];
            if !rest.is_empty() {
                self.line("", Kind::Plain);
            }
        }
    }

    fn markdown(&mut self, source: &str, prefix: &str, kind: Option<Kind>) {
        for line in markdown::render(source) {
            // Entities such as `&#27;` may have brought control characters back.
            let safe = clean(&line.text);
            let mut spans = line.spans;
            // Styling must never be able to cut a character in half.
            spans.retain(|&(a, b, _)| *safe == *line.text && safe.get(a..b).is_some());
            spans.sort_by_key(|s| s.0);
            let kind = kind.unwrap_or(match line.look {
                Look::Text => Kind::Plain,
                Look::Heading => Kind::Heading,
                Look::Code => Kind::Block,
            });
            self.line(&format!("{prefix}{}", safe.replace('\n', " ")), kind);
            let start = self.text.len() - safe.len();
            let at = |i: usize| (start + i) as u32;
            let styles = spans.into_iter().map(|(a, b, s)| (at(a), at(b), s));
            self.styles.extend(styles);
            if let Some(hang) = self.hangs.last_mut() {
                *hang = line.hang.saturating_add(prefix.width() as u8);
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

    fn file(&mut self, file: &File, source: &Source, rendered: bool) {
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
        let mut left: Vec<(usize, &Thread)> = source
            .threads
            .iter()
            .enumerate()
            .filter(|(_, t)| t.path == file.path)
            .collect();
        // Discussions without a line here come first.
        let lines: Vec<(Option<u32>, Option<u32>)> =
            file.lines.iter().map(|(k, t)| numbers(*k, t)).collect();
        let here = |t: &Thread, (old, new): &(Option<u32>, Option<u32>)| {
            t.line.is_some() && t.line == if t.side == "LEFT" { *old } else { *new }
        };
        left.retain(|(n, t)| {
            let shown = lines.iter().any(|l| here(t, l));
            if !shown {
                self.thread(*n, t, source, rendered);
            }
            shown
        });
        let outdated = source.ai.as_ref().is_some_and(|r| r.outdated);
        let on = |f: &Finding, (old, new): &(Option<u32>, Option<u32>)| {
            !outdated && f.line.is_some() && f.line == if f.side == "LEFT" { *old } else { *new }
        };
        let found = source.ai.iter().flat_map(|r| &r.findings).enumerate();
        let mut found: Vec<(usize, &Finding)> =
            found.filter(|(_, f)| f.path == file.path).collect();
        found.retain(|(n, f)| {
            let shown = lines.iter().any(|l| on(f, l));
            if !shown {
                self.finding(*n, f, rendered);
            }
            shown
        });
        for ((kind, text), at) in file.lines.iter().zip(&lines) {
            self.line(text, *kind);
            // A context line has both numbers, but a thread only one place.
            left.retain(|(n, t)| {
                let shown = here(t, at);
                if shown {
                    self.thread(*n, t, source, rendered);
                }
                !shown
            });
            found.retain(|(n, f)| {
                let shown = on(f, at);
                if shown {
                    self.finding(*n, f, rendered);
                }
                !shown
            });
        }
    }

    fn finding(&mut self, n: usize, finding: &Finding, rendered: bool) {
        self.findings.push((self.kinds.len() as u32, n));
        let (mark, kind) = if finding.accepted {
            ("✓ accepted", Kind::Accepted)
        } else {
            ("not accepted", Kind::Finding)
        };
        let mut title = format!(
            "{}┏ ◆ AI · {} · {mark} · {}",
            " ".repeat(GUTTER),
            finding.severity,
            finding.title
        );
        if finding.line.is_none() {
            title += " (not on a changed line)";
        }
        self.line(&clean(&title).replace('\n', " "), kind);
        if let Some(hang) = self.hangs.last_mut() {
            *hang = (GUTTER + 2) as u8;
        }
        self.note(&finding.body, rendered);
    }

    /// Body of a discussion or finding.
    fn note(&mut self, body: &str, rendered: bool) {
        let bar = " ".repeat(GUTTER) + "┃ ";
        let body = clean(body);
        if rendered {
            return self.markdown(&body, &bar, Some(Kind::Note));
        }
        for line in body.split('\n') {
            self.line(&format!("{bar}{line}"), Kind::Note);
            if let Some(hang) = self.hangs.last_mut() {
                *hang = (GUTTER + 2) as u8;
            }
        }
    }

    fn thread(&mut self, n: usize, thread: &Thread, source: &Source, rendered: bool) {
        let bar = " ".repeat(GUTTER) + "┃ ";
        self.threads.push((self.kinds.len() as u32, n));
        let (mark, kind) = if thread.resolved {
            ("✓ resolved", Kind::Done)
        } else {
            ("● unresolved", Kind::Open)
        };
        let mut title = format!("{}┏ {mark}", " ".repeat(GUTTER));
        if thread.outdated {
            title += " · outdated";
        }
        let short = thread.resolved && !source.show_resolved;
        if short {
            if let Some(c) = thread.comments.first() {
                let author = c.author.as_deref().unwrap_or("ghost");
                let text = c.body.lines().find(|l| !l.trim().is_empty());
                let text: String = text.unwrap_or("").chars().take(60).collect();
                title += &format!(" · @{author}: {text}");
            }
            self.line(&clean(&title).replace('\n', " "), kind);
            return;
        }
        let plural = if thread.total == 1 { "" } else { "s" };
        title += &format!(" · {} comment{plural}", thread.total);
        self.line(&title, kind);
        for c in &thread.comments {
            let author = c.author.as_deref().unwrap_or("ghost");
            let date = c.at.get(..10).unwrap_or("");
            let start = self.text.len() + 1;
            let who = clean(author).replace('\n', " ");
            self.line(&format!("{bar}@{who} · {date}"), Kind::Note);
            let from = start + bar.len();
            let to = from + 1 + who.len();
            self.styles.push((from as u32, to as u32, markdown::BOLD));
            self.note(&c.body, rendered);
        }
        if thread.total > thread.comments.len() {
            let more = thread.total - thread.comments.len();
            self.line(&format!("{bar}… {more} more on GitHub"), Kind::Note);
        }
    }

    pub fn has_threads(&self) -> bool {
        !self.threads.is_empty() || !self.findings.is_empty()
    }

    /// The discussion under the cursor.
    pub fn thread_at_cursor(&self) -> Option<usize> {
        let line = self.cursor?;
        self.threads.iter().find(|t| t.0 == line).map(|t| t.1)
    }

    /// The AI finding under the cursor.
    pub fn finding_at_cursor(&self) -> Option<usize> {
        let line = self.cursor?;
        self.findings.iter().find(|f| f.0 == line).map(|f| f.1)
    }

    /// Move the cursor to a finding, or to the first one.
    pub fn go_to_finding(&mut self, n: Option<usize>, height: usize) -> bool {
        let found = self.findings.iter().find(|f| n.is_none_or(|n| f.1 == n));
        match found.map(|f| f.0) {
            Some(line) => {
                self.put(line, height);
                true
            }
            None => false,
        }
    }

    fn put(&mut self, line: u32, height: usize) {
        self.cursor = Some(line);
        self.anchor = None;
        // Near the top, so that the whole text can be read.
        let row = self.rows.partition_point(|r| r.line < line);
        self.top = row.saturating_sub(height / 4);
        self.scroll(0, height);
    }

    fn is_stop(&self, line: u32) -> bool {
        self.kind(line).is_target() && self.findings_line != Some(line)
    }

    fn kind(&self, line: u32) -> Kind {
        let kind = self.kinds.get(line as usize).copied();
        kind.unwrap_or(Kind::Plain)
    }

    fn line_text(&self, line: u32) -> &str {
        let row = self.rows.partition_point(|r| r.line < line);
        let start = self.rows.get(row).map_or(0, |r| r.start as usize);
        self.text[start..].split('\n').next().unwrap_or("")
    }

    /// Put the cursor on the first line of code that is on screen.
    pub fn start_cursor(&mut self, height: usize) -> bool {
        let top = self.rows.get(self.top).map_or(0, |r| r.line);
        let end = self.kinds.len() as u32;
        let target = |l: &u32| self.is_stop(*l);
        let found = (top..end)
            .find(target)
            .or_else(|| (0..top).rev().find(target));
        self.cursor = found;
        self.anchor = None;
        self.reveal(height);
        found.is_some()
    }

    pub fn move_cursor(&mut self, by: isize, height: usize) {
        let Some(mut line) = self.cursor else { return };
        let end = self.kinds.len() as u32;
        for _ in 0..by.unsigned_abs().min(self.kinds.len()) {
            let target = |l: &u32| self.is_stop(*l);
            let next = if by > 0 {
                (line + 1..end).find(target)
            } else {
                (0..line).rev().find(target)
            };
            match next {
                Some(next) => line = next,
                None => break,
            }
        }
        self.cursor = Some(line);
        self.reveal(height);
    }

    /// Move the cursor to the next (or previous) discussion.
    pub fn jump_thread(&mut self, forward: bool, height: usize) -> bool {
        let top = self.rows.get(self.top).map(|r| r.line);
        let mut lines: Vec<u32> = self.threads.iter().map(|t| t.0).collect();
        lines.extend(self.findings.iter().map(|f| f.0));
        lines.sort_unstable();
        let lines = lines.iter().copied();
        let to = match (self.cursor, forward) {
            (Some(at), true) => lines.clone().find(|l| *l > at),
            (Some(at), false) => lines.clone().rev().find(|l| *l < at),
            (None, true) => lines.clone().find(|l| Some(*l) >= top),
            (None, false) => lines.clone().rev().find(|l| Some(*l) < top),
        };
        // Wrap around, so that one key reaches them all.
        let to = to.or_else(|| {
            let mut lines = lines;
            if forward {
                lines.next()
            } else {
                lines.next_back()
            }
        });
        if let Some(line) = to {
            self.put(line, height);
        }
        to.is_some()
    }

    /// What the cursor is at, to find it again in a newer document.
    pub fn mark(&self) -> Option<Mark> {
        if let Some(n) = self.finding_at_cursor() {
            return Some(Mark::Finding(n));
        }
        match self.thread_at_cursor() {
            Some(n) => Some(Mark::Thread(n)),
            None => {
                let line = self.cursor?;
                let (old, new) = numbers(self.kind(line), self.line_text(line));
                Some(Mark::Line(self.file_of(line)?.to_string(), old, new))
            }
        }
    }

    /// Put the cursor back; on the nearest line if the place is gone.
    pub fn seek(&mut self, mark: &Mark, height: usize) {
        let found = match mark {
            Mark::Thread(n) => self.threads.iter().find(|t| t.1 == *n).map(|t| t.0),
            Mark::Finding(n) => self.findings.iter().find(|f| f.1 == *n).map(|f| f.0),
            Mark::Line(path, old, new) => (0..self.kinds.len() as u32).find(|l| {
                let kind = self.kind(*l);
                kind.is_code()
                    && numbers(kind, self.line_text(*l)) == (*old, *new)
                    && self.file_of(*l) == Some(path)
            }),
        };
        if found.is_some() {
            self.cursor = found;
            self.reveal(height);
        } else {
            self.start_cursor(height);
        }
    }

    /// Scroll just enough to show the cursor with some lines around it.
    fn reveal(&mut self, height: usize) {
        let Some(line) = self.cursor else { return };
        let row = self.rows.partition_point(|r| r.line < line);
        let margin = (height / 4).min(6);
        if row < self.top + margin {
            self.top = row.saturating_sub(margin);
        } else if row + margin >= self.top + height {
            self.top = row + margin + 1 - height.min(row + margin + 1);
        }
        self.scroll(0, height);
    }

    fn file_of(&self, line: u32) -> Option<&str> {
        let n = self.files.partition_point(|f| f.0 <= line);
        self.files.get(n.checked_sub(1)?).map(|f| f.1.as_str())
    }

    /// What a comment at the cursor would be about.
    pub fn place(&self) -> Result<Place, &'static str> {
        let cursor = self.cursor.ok_or("no line is selected")?;
        let at = |line: u32| -> Option<(u32, &'static str)> {
            let kind = self.kind(line);
            let (old, new) = numbers(kind, self.line_text(line));
            match kind {
                Kind::Del => Some((old?, "LEFT")),
                Kind::Add | Kind::Context => Some((new?, "RIGHT")),
                _ => None,
            }
        };
        let (first, last) = match self.anchor {
            Some(anchor) => (anchor.min(cursor), anchor.max(cursor)),
            None => (cursor, cursor),
        };
        let path = self.file_of(last).ok_or("this line is not in a file")?;
        if self.file_of(first) != Some(path) {
            return Err("the lines must be in the same file");
        }
        if (first..last).any(|l| self.kind(l) == Kind::Hunk) {
            return Err("the lines must be in the same part of the file");
        }
        let (line, side) = at(last).ok_or("choose a line of code")?;
        let start = match first < last {
            true => Some(at(first).ok_or("choose lines of code")?),
            false => None,
        };
        Ok(Place {
            path: path.to_string(),
            line,
            side: side.to_string(),
            start_line: start.map(|s| s.0),
            start_side: start.map(|s| s.1.to_string()),
        })
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
                    if words && c == ' ' {
                        // A space at the edge stays there, out of sight.
                        space = Some((offset + i + 1, used));
                        continue;
                    }
                    let (end, kept) = match space {
                        Some((end, columns)) if words => (end, used - columns),
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
            let selected = self.cursor.is_some_and(|c| {
                let a = self.anchor.unwrap_or(c);
                (a.min(c)..=a.max(c)).contains(&r.line)
            });
            Visible {
                selected,
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

/// Old and new line number of a code line.
fn numbers(kind: Kind, text: &str) -> (Option<u32>, Option<u32>) {
    let columns = text.split('│').next().unwrap_or("");
    let mut columns = columns.split_whitespace().map(|n| n.parse().ok());
    let mut next = || columns.next().flatten();
    match kind {
        Kind::Add => (None, next()),
        Kind::Del => (next(), None),
        Kind::Context => (next(), next()),
        _ => (None, None),
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

/// Undo git's quoting of unusual paths: `"b/caf\303\251"`.
fn unquote(path: &str) -> String {
    let Some(inner) = path.strip_prefix('"').and_then(|p| p.strip_suffix('"')) else {
        return path.to_string();
    };
    let mut out = Vec::new();
    let mut bytes = inner.bytes().peekable();
    while let Some(b) = bytes.next() {
        if b != b'\\' {
            out.push(b);
            continue;
        }
        match bytes.next() {
            Some(first @ b'0'..=b'7') => {
                let mut value = u32::from(first - b'0');
                for _ in 0..2 {
                    if let Some(digit @ b'0'..=b'7') = bytes.peek().copied() {
                        value = value * 8 + u32::from(digit - b'0');
                        bytes.next();
                    }
                }
                out.push(value as u8);
            }
            Some(b't') => out.push(b'\t'),
            Some(b'n') => out.push(b'\n'),
            Some(other) => out.push(other),
            None => {}
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A changed file as text with line numbers, and which numbers it has.
pub struct Changed {
    pub path: String,
    pub text: String,
    /// Old and new line numbers, in order.
    pub old: Vec<u32>,
    pub new: Vec<u32>,
}

pub fn changed(diff: &str) -> Vec<Changed> {
    let files = parse(&clean(diff)).into_iter();
    files
        .map(|file| {
            let (mut old, mut new) = (Vec::new(), Vec::new());
            let mut text = String::new();
            for (kind, line) in &file.lines {
                let (a, b) = numbers(*kind, line);
                old.extend(a);
                new.extend(b);
                text.push_str(line);
                text.push('\n');
            }
            Changed {
                path: file.path,
                text,
                old,
                new,
            }
        })
        .collect()
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
            let path = match names.rsplit_once(" \"b/") {
                Some((_, path)) => unquote(&format!("\"{path}")),
                None => names.rsplit_once(" b/").map_or(names, |n| n.1).to_string(),
            };
            files.push(File {
                path,
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
                // Not code, such as the note that a long diff was cut.
                Some(first) if *first != b' ' => {
                    file.lines.push((Kind::Hunk, line.to_string()));
                    continue;
                }
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
        } else if line.starts_with("+++ \"b/") {
            let path = unquote(line[4..].trim_end());
            file.path = path.strip_prefix("b/").unwrap_or(&path).to_string();
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
            ..Source::default()
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
        // The space after "two," is exactly past the edge.
        let mut doc = Doc::new(&Source::preview("T\n\n- one two, three"), false);
        let got = rows(&mut doc, 10);
        assert_eq!(got[2..], ["• one two, ", "  three"]);
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

    fn thread(path: &str, line: Option<u32>, side: &str, body: &str) -> Thread {
        Thread {
            path: path.into(),
            line,
            side: side.into(),
            total: 1,
            comments: vec![crate::review::Comment {
                author: Some("al".into()),
                at: "2026-01-02T".into(),
                body: body.into(),
                ..Default::default()
            }],
            ..Thread::default()
        }
    }

    fn reviewed(threads: Vec<Thread>) -> Doc {
        let source = Source {
            diff: Some(DIFF.into()),
            markdown: true,
            threads,
            ..Source::default()
        };
        let mut doc = Doc::new(&source, false);
        doc.layout(80);
        doc
    }

    fn all(doc: &Doc) -> Vec<String> {
        doc.visible(999).map(|r| text(&r)).collect()
    }

    #[test]
    fn discussions_sit_under_their_line() {
        let mut gone = thread("src/a b.rs", None, "RIGHT", "old talk");
        gone.outdated = true;
        let mut done = thread("gone", Some(1), "LEFT", "**fixed**\nlong");
        done.resolved = true;
        let doc = reviewed(vec![
            thread("src/a b.rs", Some(2), "LEFT", "on **del**"),
            thread("src/a b.rs", Some(3), "RIGHT", "on end"),
            gone,
            done,
            thread("elsewhere", Some(1), "RIGHT", "other"),
        ]);
        let rows = all(&doc);
        let find = |t: &str| rows.iter().position(|r| r.contains(t)).unwrap();
        assert_eq!(find("--- not a header") + 3, find("on del"));
        assert!(find("on del") < find("+++ not a header"));
        assert_eq!(find("│ end") + 3, find("on end"));
        assert!(find("old talk") < find("│ keep"));
        assert!(find("bye") < find("✓ resolved · @al: **fixed**"));
        assert!(!rows.iter().any(|r| r.trim_end().ends_with("┃ long")));
        assert!(find("Discussions on other files") < find("┃ other"));
        assert_eq!(doc.threads.len(), 5);
    }

    #[test]
    fn cursor_finds_places_and_ranges() {
        let mut doc = reviewed(vec![thread("src/a b.rs", Some(3), "RIGHT", "talk")]);
        assert!(doc.start_cursor(50));
        let place = doc.place().unwrap();
        assert_eq!((place.line, place.side.as_str()), (1, "RIGHT"));
        doc.move_cursor(1, 50);
        let place = doc.place().unwrap();
        assert_eq!((place.line, place.side.as_str()), (2, "LEFT"));
        doc.anchor = doc.cursor;
        doc.move_cursor(2, 50);
        let place = doc.place().unwrap();
        assert_eq!(place.label(), "src/a b.rs:2-3");
        assert_eq!(place.start_side.as_deref(), Some("LEFT"));
        // The discussion is a stop too, but not a place to comment.
        doc.move_cursor(1, 50);
        assert_eq!(doc.thread_at_cursor(), Some(0));
        assert!(doc.place().is_err());
        doc.move_cursor(99, 50);
        assert!(doc.place().is_err(), "another file");
        doc.anchor = None;
        let place = doc.place().unwrap();
        assert_eq!((place.path.as_str(), place.side.as_str()), ("gone", "LEFT"));
    }

    #[test]
    fn jumping_between_discussions_wraps() {
        let mut doc = reviewed(vec![
            thread("src/a b.rs", Some(1), "RIGHT", "one"),
            thread("gone", Some(1), "LEFT", "two"),
        ]);
        let mut seen = Vec::new();
        for forward in [true, true, true, false] {
            assert!(doc.jump_thread(forward, 9));
            seen.push(doc.thread_at_cursor().unwrap());
        }
        assert_eq!(seen, [0, 1, 0, 1]);
        assert!(!reviewed(vec![]).jump_thread(true, 9));
    }

    #[test]
    fn unquotes_git_paths() {
        assert_eq!(unquote("plain"), "plain");
        assert_eq!(unquote(r#""b/vi\341\273\207t \"x\"""#), "b/việt \"x\"");
        let diff = "diff --git \"a/\\303\\251\" \"b/\\303\\251\"\nnew file mode 1\n--- /dev/null\n+++ \"b/\\303\\251\"\n@@ -0,0 +1 @@\n+x\n";
        assert_eq!(parse(diff)[0].path, "é");
    }

    /// Every combination of these pieces, at several widths and both modes.
    #[test]
    fn odd_markdown_never_panics() {
        let pieces = [
            "- ```\n  việt code\n  ```\n",
            "-     indented\n",
            "> - [x] ```x\n>   é\n>   ```\n",
            "1. > quote **b•ld**\n",
            "- [ ] ☐ `c•de` *i*\n  - ```\n    日本語\n    ```\n",
            "| é | `•` |\n|---|---|\n| **日** | x |\n",
            "# h **é** `•`\n",
            "<details><summary>é &amp; •</summary>\n\n- a\n\n</details>\n",
            "![é](u) [l•nk](x) ~~no~~ &#27;[31m&#x202e;\u{200b}\t\u{1f600}\n",
            "```\n• unterminated\n",
            "\n",
        ];
        let check = |source: &Source, raw: bool| {
            for width in [2, 7, 30, 83] {
                let mut doc = Doc::new(source, raw);
                doc.layout(width);
                for row in doc.visible(usize::MAX) {
                    let _ = text(&row);
                }
            }
        };
        for a in pieces {
            for b in pieces {
                for c in pieces {
                    let body = format!("{a}{b}{c}");
                    let mut source = Source::preview(&format!("T\n\n{body}"));
                    source.diff = Some(DIFF.into());
                    source.threads = vec![thread("src/a b.rs", Some(3), "RIGHT", &body)];
                    check(&source, false);
                    check(&source, true);
                }
            }
        }
    }

    #[test]
    fn long_line_numbers_are_read_in_full() {
        let diff =
            "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -123456,2 +1234567,2 @@\n keep\n-old\n+new\n";
        let source = Source {
            diff: Some(diff.into()),
            threads: vec![thread("f", Some(1_234_568), "RIGHT", "here")],
            ..Source::default()
        };
        let mut doc = Doc::new(&source, false);
        doc.layout(80);
        doc.start_cursor(40);
        let mut places = Vec::new();
        for _ in 0..3 {
            let place = doc.place().unwrap();
            places.push((place.line, place.side));
            doc.move_cursor(1, 40);
        }
        let want = [
            (1_234_567, "RIGHT"),
            (123_457, "LEFT"),
            (1_234_568, "RIGHT"),
        ];
        assert_eq!(places, want.map(|(n, s)| (n, s.to_string())));
        let rows = all(&doc);
        let new = rows.iter().position(|r| r.contains("+new")).unwrap();
        assert!(rows[new + 1].contains("unresolved"));
    }

    #[test]
    fn a_cut_diff_ends_with_a_note() {
        let diff =
            "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,9 +1,9 @@\n a\n+b\n\u{2026} truncated";
        let file = &parse(diff)[0];
        assert_eq!(file.lines.last().unwrap().0, Kind::Hunk);
        assert_eq!((file.added, file.deleted), (1, 0));
    }

    /// Diffs cut, shuffled and damaged at every place.
    #[test]
    fn damaged_diffs_never_panic() {
        let mut texts: Vec<String> = DIFF
            .char_indices()
            .flat_map(|(i, _)| [DIFF[..i].to_string(), DIFF[i..].to_string()])
            .collect();
        for junk in [
            "@@ -1 +1 @@\n",
            "@@ -x,y +z @@\n",
            "@@ -0,0 +4294967295,9 @@\n+é\n",
            "+++ \"b/\\9\\\n",
            "é\n",
            "\\\n",
        ] {
            texts.push(format!("{junk}{DIFF}{junk}{DIFF}"));
            texts.push(DIFF.replace('\n', &format!("\n{junk}")));
        }
        for text in texts {
            let source = Source {
                diff: Some(text),
                threads: vec![thread("gone", Some(1), "LEFT", "x")],
                ..Source::default()
            };
            for width in [2, 13, 60] {
                let mut doc = Doc::new(&source, false);
                doc.layout(width);
                doc.start_cursor(9);
                for by in [1, 5, -3, 99] {
                    doc.move_cursor(by, 9);
                    let _ = (doc.place(), doc.mark(), doc.current_file());
                    doc.jump(by > 0, 9);
                    doc.jump_thread(by > 0, 9);
                    doc.anchor = doc.cursor;
                }
            }
        }
    }

    #[test]
    fn findings_sit_under_their_line_and_are_stops() {
        let finding = |path: &str, line: Option<u32>, title: &str| Finding {
            path: path.into(),
            line,
            side: "RIGHT".into(),
            severity: "major".into(),
            title: title.into(),
            body: "**b** \u{1b}[2J".into(),
            accepted: title == "two",
        };
        let mut source = Source {
            head: "T".into(),
            diff: Some(DIFF.into()),
            markdown: true,
            threads: vec![thread("src/a b.rs", Some(1), "RIGHT", "talk")],
            ai: Some(Review {
                findings: vec![
                    finding("src/a b.rs", Some(3), "one"),
                    finding("src/a b.rs", None, "two"),
                    finding("nope", None, "three"),
                ],
                ..Review::default()
            }),
            ..Source::default()
        };
        let mut doc = Doc::new(&source, false);
        doc.layout(120);
        let rows = all(&doc);
        assert!(!rows.concat().contains('\u{1b}'));
        let find = |t: &str| rows.iter().position(|r| r.contains(t)).unwrap();
        assert_eq!(find("│ end") + 1, find("not accepted · one"));
        assert!(find("accepted · two") < find("│ keep"));
        assert!(find("· three") > find("gone  (deleted)"));
        assert!(rows[1].contains("3 findings, 1 accepted"));
        // Discussions and findings are visited in order.
        let mut seen = Vec::new();
        for _ in 0..4 {
            doc.jump_thread(true, 9);
            seen.push((doc.thread_at_cursor(), doc.finding_at_cursor()));
        }
        let want = [
            (None, Some(1)),
            (Some(0), None),
            (None, Some(0)),
            (None, Some(2)),
        ];
        assert_eq!(seen, want);
        assert!(doc.place().is_err());
        doc.start_cursor(99);
        assert!(doc.finding_at_cursor().is_some() || doc.place().is_ok());
        // Outdated findings leave their lines.
        if let Some(ai) = &mut source.ai {
            ai.outdated = true;
        }
        let mut doc = Doc::new(&source, false);
        doc.layout(120);
        let rows = all(&doc);
        let find = |t: &str| rows.iter().position(|r| r.contains(t)).unwrap();
        assert!(find("· one") < find("│ keep"));
    }

    #[test]
    fn changed_lists_the_lines_of_the_diff() {
        let files = changed(DIFF);
        assert_eq!(
            (&files[0].old, &files[0].new),
            (&vec![1, 2, 3], &vec![1, 2, 3])
        );
        assert_eq!((&files[2].old, &files[2].new), (&vec![1], &vec![]));
    }
}
