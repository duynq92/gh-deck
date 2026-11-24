//! Turns GitHub Markdown into styled lines for the terminal.

use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use unicode_width::UnicodeWidthStr;

pub const BOLD: u8 = 1;
pub const ITALIC: u8 = 2;
pub const CODE: u8 = 4;
pub const LINK: u8 = 8;
pub const STRIKE: u8 = 16;
pub const DIM: u8 = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Look {
    Text,
    Heading,
    Code,
}

#[derive(Debug, PartialEq)]
pub struct Line {
    pub look: Look,
    /// Columns a wrapped line is indented.
    pub hang: u8,
    pub text: String,
    /// Byte range and style bits.
    pub spans: Vec<(usize, usize, u8)>,
}

#[derive(Default)]
struct Render {
    lines: Vec<Line>,
    text: String,
    spans: Vec<(usize, usize, u8)>,
    style: u8,
    /// Open styles, to restore the outer one.
    styles: Vec<u8>,
    heading: bool,
    code: bool,
    quotes: usize,
    /// Next number of every open list; `None` for bullets.
    lists: Vec<Option<u64>>,
    /// Marker of the item that has no text yet.
    marker: Option<String>,
    html: Option<String>,
    /// Rows of the open table.
    table: Option<Vec<Vec<String>>>,
    cell: Option<String>,
}

pub fn render(source: &str) -> Vec<Line> {
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS | Options::ENABLE_STRIKETHROUGH;
    let mut r = Render::default();
    for event in Parser::new_ext(source, options) {
        r.event(event);
    }
    r.flush();
    while r.lines.last().is_some_and(|l| l.text.trim().is_empty()) {
        r.lines.pop();
    }
    r.lines
}

impl Render {
    fn event(&mut self, event: Event) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => match &mut self.html {
                Some(html) => html.push_str(&text),
                None => self.push(&text),
            },
            Event::Code(text) => self.styled(CODE, &text),
            Event::Html(text) => match &mut self.html {
                Some(html) => html.push_str(&text),
                None => self.push(&strip_html(&text)),
            },
            Event::InlineHtml(tag) => {
                if tag.to_ascii_lowercase().starts_with("<br") {
                    self.newline();
                } else {
                    let alt = strip_html(&tag); // only images leave text
                    self.push(&alt);
                }
            }
            // GitHub shows line breaks in comments and descriptions.
            Event::SoftBreak | Event::HardBreak => self.newline(),
            Event::Rule => {
                self.blank();
                self.styled(DIM, &"─".repeat(40));
                self.flush();
            }
            Event::TaskListMarker(done) => {
                self.marker = Some(if done { "☑ " } else { "☐ " }.to_string());
            }
            Event::InlineMath(text) | Event::DisplayMath(text) => self.styled(CODE, &text),
            Event::FootnoteReference(name) => self.push(&format!("[{name}]")),
        }
    }

    fn open(&mut self, style: u8) {
        self.styles.push(self.style);
        self.style |= style;
    }

    fn close(&mut self) {
        self.style = self.styles.pop().unwrap_or_default();
    }

    fn start(&mut self, tag: Tag) {
        match tag {
            Tag::Paragraph => {
                // The first paragraph of an item stays next to its marker.
                if self.marker.is_none() {
                    self.blank();
                }
            }
            Tag::Heading { level, .. } => {
                self.blank();
                self.heading = true;
                if level == HeadingLevel::H1 {
                    self.push("█ ");
                }
            }
            Tag::BlockQuote(_) => {
                self.blank();
                self.quotes += 1;
            }
            Tag::CodeBlock(_) => {
                self.flush();
                self.blank();
                self.code = true;
            }
            Tag::HtmlBlock => {
                self.flush();
                self.html = Some(String::new());
            }
            Tag::List(first) => {
                self.flush();
                if self.lists.is_empty() {
                    self.blank();
                }
                self.lists.push(first);
            }
            Tag::Item => {
                self.flush();
                self.marker = Some(match self.lists.last_mut() {
                    Some(Some(n)) => {
                        *n += 1;
                        format!("{}. ", *n - 1)
                    }
                    _ => "• ".to_string(),
                });
            }
            Tag::Table(_) => {
                self.flush();
                self.blank();
                self.table = Some(Vec::new());
            }
            Tag::TableHead | Tag::TableRow => {
                if let Some(rows) = &mut self.table {
                    rows.push(Vec::new());
                }
            }
            Tag::TableCell => self.cell = Some(String::new()),
            Tag::Emphasis => self.open(ITALIC),
            Tag::Strong => self.open(BOLD),
            Tag::Strikethrough => self.open(STRIKE),
            Tag::Link { .. } => self.open(LINK),
            Tag::Image { .. } => {
                self.open(DIM);
                self.push("[image: ");
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => self.flush(),
            TagEnd::Heading(_) => {
                self.flush();
                self.heading = false;
            }
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.quotes = self.quotes.saturating_sub(1);
            }
            TagEnd::CodeBlock => {
                // The block ends with a newline, so nothing is left to flush.
                self.flush();
                self.code = false;
            }
            TagEnd::HtmlBlock => {
                let html = self.html.take().unwrap_or_default();
                let text = strip_html(&html);
                if !text.trim().is_empty() {
                    self.blank();
                }
                for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
                    self.push(line);
                    self.flush();
                }
            }
            TagEnd::List(_) => {
                self.flush();
                self.lists.pop();
            }
            TagEnd::Item => {
                self.flush();
                // An item without text still shows its marker.
                if self.marker.is_some() {
                    self.push(" ");
                    self.flush();
                }
            }
            TagEnd::TableCell => {
                let cell = self.cell.take().unwrap_or_default();
                if let Some(row) = self.table.as_mut().and_then(|t| t.last_mut()) {
                    row.push(cell);
                }
            }
            TagEnd::Table => {
                let rows = self.table.take().unwrap_or_default();
                self.draw_table(&rows);
            }
            TagEnd::Image => {
                self.push("]");
                self.close();
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough | TagEnd::Link => {
                self.close();
            }
            _ => {}
        }
    }

    fn styled(&mut self, style: u8, text: &str) {
        self.open(style);
        self.push(text);
        self.close();
    }

    fn push(&mut self, text: &str) {
        if let Some(cell) = &mut self.cell {
            cell.push_str(&text.replace('\n', " "));
            return;
        }
        for (n, part) in text.split('\n').enumerate() {
            if n > 0 {
                self.newline();
            }
            let start = self.text.len();
            self.text.push_str(part);
            if self.style != 0 && !part.is_empty() {
                self.spans.push((start, self.text.len(), self.style));
            }
        }
    }

    fn newline(&mut self) {
        if let Some(cell) = &mut self.cell {
            cell.push(' ');
        } else if self.code || !self.text.is_empty() {
            self.emit();
        }
    }

    /// Finish the current line, if it has any text.
    fn flush(&mut self) {
        if !self.text.is_empty() {
            self.emit();
        }
    }

    /// One empty line between blocks, not at the start.
    fn blank(&mut self) {
        self.flush();
        let inside = !self.lists.is_empty();
        if inside || self.lines.last().is_none_or(|l| l.text.is_empty()) {
            return;
        }
        self.lines.push(Line {
            look: Look::Text,
            hang: 0,
            text: String::new(),
            spans: Vec::new(),
        });
    }

    fn emit(&mut self) {
        let mut prefix = "│ ".repeat(self.quotes);
        let quote = prefix.len();
        let depth = self.lists.len().saturating_sub(1);
        let marker = self.marker.take();
        if !self.lists.is_empty() {
            prefix.push_str(&"  ".repeat(depth));
            match &marker {
                Some(marker) => prefix.push_str(marker),
                None => prefix.push_str("  "),
            }
        }
        if self.code {
            prefix.push_str("  ");
        }
        let at = prefix.len();
        let mut spans = Vec::with_capacity(self.spans.len() + 2);
        if quote > 0 {
            spans.push((0, quote, DIM));
        }
        if let Some(marker) = &marker {
            spans.push((at - marker.len(), at, BOLD));
        }
        spans.extend(self.spans.drain(..).map(|(a, b, s)| (a + at, b + at, s)));
        self.lines.push(Line {
            look: match (self.code, self.heading) {
                (true, _) => Look::Code,
                (_, true) => Look::Heading,
                _ => Look::Text,
            },
            hang: u8::try_from(prefix.width()).unwrap_or(u8::MAX),
            text: prefix + &std::mem::take(&mut self.text),
            spans,
        });
    }

    fn draw_table(&mut self, rows: &[Vec<String>]) {
        let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
        let widths: Vec<usize> = (0..columns)
            .map(|c| {
                let cells = rows.iter().filter_map(|r| r.get(c));
                cells.map(|t| t.trim().width()).max().unwrap_or(0)
            })
            .collect();
        for (n, row) in rows.iter().enumerate() {
            for (c, width) in widths.iter().enumerate() {
                let cell = row.get(c).map_or("", |t| t.trim());
                if c > 0 {
                    self.styled(DIM, " │ ");
                }
                self.styled(if n == 0 { BOLD } else { 0 }, cell);
                if c + 1 < columns {
                    self.push(&" ".repeat(width - cell.width()));
                }
            }
            self.flush();
            if n == 0 {
                let rule: Vec<String> = widths.iter().map(|w| "─".repeat(*w)).collect();
                self.styled(DIM, &rule.join("─┼─"));
                self.flush();
            }
        }
    }
}

/// Text of an HTML fragment: no tags, no comments, images as their description.
fn strip_html(html: &str) -> String {
    let mut out = String::new();
    let mut rest = html;
    while let Some(open) = rest.find('<') {
        out.push_str(&rest[..open]);
        rest = &rest[open..];
        let close = if rest.starts_with("<!--") {
            rest.find("-->").map(|i| i + 3)
        } else {
            rest.find('>').map(|i| i + 1)
        };
        let Some(close) = close else {
            rest = ""; // an unfinished tag
            break;
        };
        let tag = &rest[..close];
        let name = tag[1..].trim_start_matches('/').to_ascii_lowercase();
        if name.starts_with("img") {
            let alt = tag.split("alt=\"").nth(1).and_then(|t| t.split('"').next());
            out.push_str(&match alt {
                Some(alt) if !alt.is_empty() => format!("[image: {alt}]"),
                _ => "[image]".to_string(),
            });
        }
        let breaks = [
            "br", "p", "div", "li", "tr", "summary", "details", "h1", "h2", "h3",
        ];
        let word: String = name
            .chars()
            .take_while(char::is_ascii_alphanumeric)
            .collect();
        if breaks.contains(&word.as_str()) {
            out.push('\n');
        }
        rest = &rest[close..];
    }
    out.push_str(rest);
    // `&amp;` last, so that `&amp;lt;` stays `&lt;`.
    for (entity, text) in [
        ("&nbsp;", " "),
        ("&lt;", "<"),
        ("&gt;", ">"),
        ("&quot;", "\""),
        ("&#39;", "'"),
        ("&amp;", "&"),
    ] {
        if out.contains(entity) {
            out = out.replace(entity, text);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(source: &str) -> Vec<String> {
        render(source).into_iter().map(|l| l.text).collect()
    }

    #[test]
    fn renders_blocks() {
        let source = "# Top\nintro\nnext\n\n- [x] done\n- [ ] open\n  - nested\n\n1. one\n2. two\n\n\
            > quoted\n\n```rust\nlet a = 1;\n\nlet b;\n```\n---\nend";
        assert_eq!(
            text(source),
            [
                "█ Top",
                "",
                "intro",
                "next",
                "",
                "☑ done",
                "☐ open",
                "  • nested",
                "",
                "1. one",
                "2. two",
                "",
                "│ quoted",
                "",
                "  let a = 1;",
                "  ",
                "  let b;",
                "",
                &"─".repeat(40),
                "",
                "end",
            ]
        );
    }

    #[test]
    fn keeps_inline_styles_in_range() {
        let lines = render("a **b `c`** [link](http://x) ~~no~~ việt *đậm*");
        assert_eq!(lines[0].text, "a b c link no việt đậm");
        let got: Vec<(&str, u8)> = lines[0]
            .spans
            .iter()
            .map(|&(a, b, s)| (&lines[0].text[a..b], s))
            .collect();
        assert_eq!(
            got,
            [
                ("b ", BOLD),
                ("c", BOLD | CODE),
                ("link", LINK),
                ("no", STRIKE),
                ("đậm", ITALIC)
            ]
        );
    }

    #[test]
    fn aligns_tables() {
        assert_eq!(
            text("| a | bb |\n|---|---|\n| **ccc** | d |"),
            ["a   │ bb", "────┼───", "ccc │ d"]
        );
    }

    #[test]
    fn drops_html_noise() {
        let source = "<!-- hidden\nnote -->\n<details>\n<summary>More &amp; more</summary>\n\nbody <br> x <img src=\"u\" alt=\"Pic\">\n\n</details>\n\n<a href=\"u\"><picture><img alt=\"Evidence\"></picture></a>";
        assert_eq!(
            text(source),
            [
                "More & more",
                "",
                "body ",
                " x [image: Pic]",
                "",
                "[image: Evidence]"
            ]
        );
    }

    #[test]
    fn survives_odd_input() {
        for source in [
            "", "<", "<img", "```", "- ", "|", "- [ ]", "> ", "<!--", "[", "**",
        ] {
            render(source);
        }
    }
}
