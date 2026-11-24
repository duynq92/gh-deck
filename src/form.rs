//! Modal input forms and the little text editor they use.

use crate::api::Item;
use crate::keys::{Act, Tab};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone)]
pub enum Kind {
    Text {
        buf: String,
        /// Byte offset, always on a char boundary.
        cur: usize,
        multi: bool,
    },
    Toggle(bool),
    Choice(Vec<&'static str>, usize),
}

impl Kind {
    pub fn text(value: &str, multi: bool) -> Self {
        let buf = value.replace("\r\n", "\n");
        Kind::Text {
            cur: buf.len(),
            buf,
            multi,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Field {
    pub label: &'static str,
    pub kind: Kind,
}

#[derive(Debug, Clone)]
pub struct Form {
    pub title: String,
    pub tab: Tab,
    pub act: Act,
    pub repo: String,
    pub item: Option<Item>,
    /// Action-specific data (the workflow id for `Trigger`).
    pub extra: String,
    pub fields: Vec<Field>,
    pub focus: usize,
    pub error: Option<String>,
    /// Facts to know before submitting, and whether they are good.
    pub notes: Vec<(bool, String)>,
}

fn field(label: &'static str, kind: Kind) -> Field {
    Field { label, kind }
}

impl Form {
    pub fn new(tab: Tab, act: Act, repo: &str, item: Option<Item>, extra: &str) -> Option<Form> {
        let line = |s: &str| Kind::text(s, false);
        let area = |s: &str| Kind::text(s, true);
        let (verb, fields) = match (act, &item) {
            (Act::Comment, Some(_)) => ("Comment on", vec![field("Comment", area(""))]),
            (Act::LineComment, Some(_)) => ("Comment on", vec![field("Comment", area(""))]),
            (Act::Reply, Some(_)) => ("Reply in", vec![field("Reply", area(""))]),
            (Act::Approve, Some(_)) => ("Approve", vec![field("Comment (optional)", area(""))]),
            (Act::RequestChanges, Some(_)) => {
                ("Request changes on", vec![field("Comment", area(""))])
            }
            (Act::Merge, Some(Item::Ticket(_))) => (
                "Merge",
                vec![
                    field(
                        "Method",
                        Kind::Choice(vec!["Merge commit", "Squash", "Rebase"], 0),
                    ),
                    field("Delete branch", Kind::Toggle(false)),
                ],
            ),
            (Act::Title, Some(Item::Ticket(t))) => {
                ("Title of", vec![field("Title", line(&t.title))])
            }
            (Act::Description, Some(Item::Ticket(t))) => {
                ("Description of", vec![field("Markdown", area(&t.body))])
            }
            (Act::Labels, Some(Item::Ticket(t))) => (
                "Labels of",
                vec![field("Labels, one per line", area(&t.labels.join("\n")))],
            ),
            (Act::Assignees, Some(Item::Ticket(t))) => (
                "Assignees of",
                vec![field(
                    "Logins, one per line (@me works)",
                    area(&t.assignees.join("\n")),
                )],
            ),
            (Act::New, None) if tab == Tab::Issues => (
                "New issue in",
                vec![
                    field("Title", line("")),
                    field("Body", area("")),
                    field("Labels, one per line", area("")),
                ],
            ),
            (Act::New, None) if tab == Tab::Releases => (
                "New release in",
                vec![
                    field("Tag", line("")),
                    field("Title", line("")),
                    field("Target branch (optional)", line("")),
                    field("Notes", area("")),
                    field("Generate notes", Kind::Toggle(false)),
                    field("Draft", Kind::Toggle(true)),
                    field("Prerelease", Kind::Toggle(false)),
                ],
            ),
            (Act::Edit, Some(Item::Release(r))) => (
                "Edit",
                vec![
                    field("Title", line(r.name.as_deref().unwrap_or(""))),
                    field("Notes", area(r.body.as_deref().unwrap_or(""))),
                    field("Draft", Kind::Toggle(r.draft)),
                    field("Prerelease", Kind::Toggle(r.prerelease)),
                ],
            ),
            (Act::Download, Some(Item::Release(_))) => (
                "Download assets of",
                vec![
                    field("File pattern", line("*")),
                    field("Into directory", line(".")),
                ],
            ),
            (Act::Trigger, None) => (
                "Trigger workflow in",
                vec![
                    field("Branch or tag (empty = default)", line("")),
                    field("Inputs, key=value per line", area("")),
                ],
            ),
            _ => return None,
        };
        let mut fields = fields;
        if act == Act::Description
            && let Some(Kind::Text { cur, .. }) = fields.first_mut().map(|f| &mut f.kind)
        {
            *cur = 0; // long text is read from the top
        }
        let target = item.as_ref().map_or_else(|| repo.to_string(), Item::id);
        Some(Form {
            title: format!("{verb} {target}"),
            tab,
            act,
            repo: repo.to_string(),
            item,
            extra: extra.to_string(),
            fields,
            focus: 0,
            error: None,
            notes: Vec::new(),
        })
    }

    pub fn text(&self, i: usize) -> &str {
        match &self.fields[i].kind {
            Kind::Text { buf, .. } => buf,
            _ => "",
        }
    }

    pub fn toggle(&self, i: usize) -> bool {
        matches!(self.fields[i].kind, Kind::Toggle(true))
    }

    /// Text of the chosen option.
    pub fn chosen(&self, i: usize) -> &'static str {
        match &self.fields[i].kind {
            Kind::Choice(all, n) => all.get(*n).copied().unwrap_or(""),
            _ => "",
        }
    }

    pub fn paste(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        if let Kind::Text { buf, cur, multi } = &mut self.fields[self.focus].kind {
            let text = if *multi {
                text
            } else {
                text.replace('\n', " ")
            };
            buf.insert_str(*cur, &text);
            *cur += text.len();
        }
    }

    pub fn key(&mut self, key: KeyEvent) {
        let count = self.fields.len();
        match key.code {
            KeyCode::Tab => self.focus = (self.focus + 1) % count,
            KeyCode::BackTab => self.focus = (self.focus + count - 1) % count,
            code => match &mut self.fields[self.focus].kind {
                Kind::Text { buf, cur, multi } => edit(buf, cur, *multi, key),
                Kind::Toggle(on) => match code {
                    KeyCode::Char(' ') | KeyCode::Enter => *on = !*on,
                    KeyCode::Down => self.focus = (self.focus + 1) % count,
                    KeyCode::Up => self.focus = (self.focus + count - 1) % count,
                    _ => {}
                },
                Kind::Choice(all, n) => match code {
                    _ if all.is_empty() => {}
                    KeyCode::Right | KeyCode::Char(' ') => *n = (*n + 1) % all.len(),
                    KeyCode::Left => *n = (*n + all.len() - 1) % all.len(),
                    _ => {}
                },
            },
        }
    }
}

/// Byte range of the line the cursor is on.
pub fn line_at(buf: &str, cur: usize) -> (usize, usize) {
    let start = buf[..cur].rfind('\n').map_or(0, |i| i + 1);
    let end = buf[cur..].find('\n').map_or(buf.len(), |i| cur + i);
    (start, end)
}

/// Byte offset of the `col`-th char in a line.
fn column(buf: &str, (start, end): (usize, usize), col: usize) -> usize {
    buf[start..end]
        .char_indices()
        .nth(col)
        .map_or(end, |(i, _)| start + i)
}

pub fn edit(buf: &mut String, cur: &mut usize, multi: bool, key: KeyEvent) {
    let prev = |buf: &str, cur: usize| buf[..cur].char_indices().next_back().map(|(i, _)| i);
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let (start, end) = line_at(buf, *cur);
    match key.code {
        KeyCode::Char('u') if ctrl => {
            buf.drain(start..*cur);
            *cur = start;
        }
        KeyCode::Char('a') if ctrl => *cur = start,
        KeyCode::Char('e') if ctrl => *cur = end,
        KeyCode::Char(_) if ctrl => {}
        KeyCode::Char(c) => {
            buf.insert(*cur, c);
            *cur += c.len_utf8();
        }
        KeyCode::Enter if multi => {
            buf.insert(*cur, '\n');
            *cur += 1;
        }
        KeyCode::Backspace => {
            if let Some(i) = prev(buf, *cur) {
                buf.remove(i);
                *cur = i;
            }
        }
        KeyCode::Delete if *cur < buf.len() => {
            buf.remove(*cur);
        }
        KeyCode::Left => *cur = prev(buf, *cur).unwrap_or(0),
        KeyCode::Right => *cur += buf[*cur..].chars().next().map_or(0, char::len_utf8),
        KeyCode::Home => *cur = start,
        KeyCode::End => *cur = end,
        KeyCode::Up if start > 0 => {
            let col = buf[start..*cur].chars().count();
            *cur = column(buf, line_at(buf, start - 1), col);
        }
        KeyCode::Down if end < buf.len() => {
            let col = buf[start..*cur].chars().count();
            *cur = column(buf, line_at(buf, end + 1), col);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_unicode_lines() {
        let (mut buf, mut cur) = ("việt\nab".to_string(), 0);
        cur += buf.len();
        let mut press = |code| edit(&mut buf, &mut cur, true, KeyEvent::from(code));
        press(KeyCode::Up); // column 2 of "việt"
        press(KeyCode::Right);
        press(KeyCode::Backspace); // removes 'ệ'
        press(KeyCode::Down);
        press(KeyCode::Enter);
        press(KeyCode::Char('z'));
        assert_eq!(buf, "vit\nab\nz");
        assert_eq!(cur, buf.len());
    }

    #[test]
    fn single_line_paste_has_no_newlines() {
        let mut form = Form::new(Tab::Issues, Act::New, "o/r", None, "").unwrap();
        form.paste("a\r\nb");
        assert_eq!(form.text(0), "a b");
        form.key(KeyEvent::from(KeyCode::Tab));
        form.paste("a\r\nb");
        assert_eq!(form.text(1), "a\nb");
    }

    #[test]
    fn random_editing_keeps_the_cursor_on_a_character() {
        let keys = [
            KeyCode::Char('a'),
            KeyCode::Char('ệ'),
            KeyCode::Char('日'),
            KeyCode::Char('😀'),
            KeyCode::Enter,
            KeyCode::Backspace,
            KeyCode::Delete,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Home,
            KeyCode::End,
        ];
        let (mut buf, mut cur) = (String::new(), 0);
        let mut seed = 7u32;
        for _ in 0..50_000 {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let n = (seed >> 16) as usize;
            let key = match n % 60 {
                0 => KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL),
                1 => KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL),
                2 => KeyEvent::new(KeyCode::Char('e'), KeyModifiers::CONTROL),
                _ => KeyEvent::from(keys[n % keys.len()]),
            };
            edit(&mut buf, &mut cur, !n.is_multiple_of(5), key);
            assert!(buf.is_char_boundary(cur), "{buf:?} {cur}");
        }
    }
}
