//! Drawing. Every label that has a shortcut shows it.

use crate::api::{self, Choice, Item, Reviews};
use crate::app::{App, Modal, Picker, Status};
use crate::doc::{GUTTER, Kind as LineKind, clean};
use crate::form::{Form, Kind, line_at};
use crate::keys::{self, Act, GLOBAL, Group, SCOPE, Tab};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const ACCENT: Color = Color::Cyan;
const KEY: Style = Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD);
const HEAD: Style = Style::new()
    .fg(Color::Gray)
    .add_modifier(Modifier::BOLD.union(Modifier::UNDERLINED));
const DIM: Style = Style::new().fg(Color::DarkGray);
const BOLD: Style = Style::new().add_modifier(Modifier::BOLD);

fn key(k: &str) -> Span<'static> {
    Span::styled(format!("[{k}]"), KEY)
}

/// `hints(&[("Esc", "Cancel")])` -> ` [Esc] Cancel `
fn hints(list: &[(&str, &str)]) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    for (k, label) in list {
        spans.push(key(k));
        spans.push(Span::raw(format!(" {label} ")));
    }
    Line::from(spans)
}

fn pane(focused: bool) -> Block<'static> {
    let style = if focused {
        Style::new().fg(ACCENT)
    } else {
        DIM
    };
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(style)
}

/// Cut to `width` columns, padding when shorter.
fn fit(text: &str, width: usize) -> String {
    let text = clean(text).replace('\n', " ");
    let mut out = String::new();
    let mut used = 0;
    let long = text.width() > width;
    let limit = if long { width.saturating_sub(1) } else { width };
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if used + w > limit {
            break;
        }
        out.push(c);
        used += w;
    }
    if long && width > 0 {
        out.push('…');
        used += 1;
    }
    out.extend(std::iter::repeat_n(' ', width - used));
    out
}

pub fn draw(frame: &mut Frame, app: &mut App) {
    let screen = frame.area();
    let rows = flow(app, screen.width, 2);
    // Boxes when they leave enough room for content, one-line groups otherwise.
    let boxed = rows.len() as u16 * 3 <= screen.height / 5;
    let rows = if boxed {
        rows
    } else {
        flow(app, screen.width, 0)
    };
    let height = rows.len() as u16 * if boxed { 3 } else { 1 };
    let [header, body, bar, status] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(height),
        Constraint::Length(1),
    ])
    .areas(screen);

    draw_header(frame, app, header);
    draw_body(frame, app, body);
    draw_actions(frame, app, bar, &rows, boxed);
    draw_status(frame, app, status);

    match &app.modal {
        Some(Modal::Help) => draw_help(frame, app.tab),
        Some(Modal::Confirm(cmd)) => draw_confirm(frame, &cmd.label),
        Some(Modal::Form(form)) => draw_form(frame, form),
        Some(Modal::Picker(p)) => draw_picker(frame, p),
        None => {}
    }
}

fn draw_body(frame: &mut Frame, app: &mut App, body: Rect) {
    let split = if body.width >= 180 {
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
    } else if body.height >= 24 {
        // Stacked panes give columns and code the full width.
        let list = if app.detail_focus { 25 } else { 55 };
        Layout::vertical([Constraint::Percentage(list), Constraint::Fill(1)])
    } else if body.width >= 120 && !app.detail_focus {
        Layout::horizontal([Constraint::Percentage(62), Constraint::Fill(1)])
    } else if app.detail_focus {
        return draw_doc(frame, app, body);
    } else {
        return draw_list(frame, app, body);
    };
    let [list, doc] = split.areas(body);
    draw_list(frame, app, list);
    draw_doc(frame, app, doc);
}

#[derive(Clone, Copy)]
enum TabSize {
    Full,
    Short,
    Tiny,
}

/// `All`, `name`, `name +2`, or just the count when there is no room.
fn summary(chosen: &[Choice], longest: usize) -> String {
    let text = match chosen {
        [] => return "All".into(),
        [one] => clean(&one.0).into_owned(),
        [first, rest @ ..] => {
            let text = format!("{} +{}", clean(&first.0), rest.len());
            if text.width() > longest {
                return chosen.len().to_string();
            }
            text
        }
    };
    fit(&text, text.width().min(longest.max(3)))
}

/// Tabs on the left, what is shown on the right, always on one row.
fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let width = area.width as usize;
    let list = app.list();
    let values: Vec<(&str, &[Choice], char)> = SCOPE
        .keys
        .iter()
        .filter_map(|k| {
            let chosen = match k.act {
                Act::Org => &app.orgs,
                Act::Repo => &app.scope,
                Act::Filter if api::filters(app.tab, &[]).is_empty() => return None,
                Act::Filter => &list.filters,
                _ if api::states(app.tab).is_empty() => return None,
                _ => &list.states,
            };
            Some((k.label, chosen.as_slice(), k.key))
        })
        .collect();
    let tabs = |size: TabSize| {
        let mut spans = Vec::new();
        for (i, tab) in Tab::ALL.into_iter().enumerate() {
            let name = match size {
                TabSize::Full => format!(" {} ", tab.title()),
                TabSize::Short => format!(" {} ", tab.short()),
                TabSize::Tiny => format!("{} ", tab.tiny()),
            };
            let style = if tab == app.tab {
                Style::new()
                    .fg(Color::Black)
                    .bg(ACCENT)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::new()
            };
            spans.push(key(&(i + 1).to_string()));
            spans.push(Span::styled(name, style));
        }
        Line::from(spans)
    };
    // Every value keeps its key and label; only the values get shorter.
    let right = |gap: &'static str, longest: usize| {
        let mut spans = Vec::new();
        for (label, chosen, k) in &values {
            spans.push(key(&k.to_string()));
            spans.push(Span::raw(format!("{gap}{label}:")));
            let style = if chosen.is_empty() {
                BOLD
            } else {
                Style::new().fg(Color::Green).add_modifier(Modifier::BOLD)
            };
            spans.push(Span::styled(summary(chosen, longest), style));
            spans.push(Span::raw(" "));
        }
        Line::from(spans)
    };
    let sizes = [
        (TabSize::Full, " ", 18),
        (TabSize::Short, " ", 12),
        (TabSize::Tiny, " ", 4),
        (TabSize::Tiny, "", 1),
    ];
    let mut best = (tabs(TabSize::Tiny), right(" ", 1));
    'fit: for (size, gap, min) in sizes {
        for longest in (min..=40).rev() {
            let (l, r) = (tabs(size), right(gap, longest));
            let fits = l.width() + r.width() < width;
            best = (l, r);
            if fits {
                break 'fit;
            }
        }
    }
    let (left, right) = best;
    frame.render_widget(left, area);
    let len = (right.width() as u16).min(area.width);
    let area = Rect::new(area.right() - len, area.y, len, area.height);
    frame.render_widget(right, area);
}

fn draw_list(frame: &mut Frame, app: &mut App, area: Rect) {
    let mut block = pane(!app.detail_focus && app.modal.is_none());
    let list = app.list();
    let count = if !list.find.is_empty() {
        format!("{}/{}", list.view.len(), list.items.len())
    } else if list.cursor.is_some() {
        format!("{} of {}", list.items.len(), list.total)
    } else {
        list.items.len().to_string()
    };
    let loading = if list.loading { " · loading…" } else { "" };
    block = block.title(Line::styled(
        format!(" {} ({count}){loading} ", app.tab.title()),
        BOLD,
    ));
    if app.finding || !list.find.is_empty() {
        let mut spans = vec![Span::raw(" "), key("/"), Span::raw(" Find: ")];
        spans.push(Span::styled(clean(&list.find).into_owned(), BOLD));
        if app.finding {
            spans.push(Span::styled("▏", KEY));
            spans.extend(hints(&[("Enter", "Keep"), ("Esc", "Clear")]).spans);
        }
        spans.push(Span::raw(" "));
        block = block.title_top(Line::from(spans).right_aligned());
    }
    block = block.title_bottom(hints(&[
        ("j/k", "Move"),
        ("g/G", "Top/End"),
        ("Enter", "Open"),
    ]));
    let mut inner = block.inner(area);
    frame.render_widget(block, area);
    let columns = Columns::new(app.tab, inner.width as usize, app.scope.len() == 1);
    if inner.height > 3 {
        let header = Rect { height: 1, ..inner };
        frame.render_widget(columns.header(app.tab).style(HEAD), header);
        inner.y += 1;
        inner.height -= 1;
    }
    let height = inner.height as usize;
    app.list_height = height;

    let list = &mut app.lists[app.tab as usize];
    list.top = list
        .top
        .min(list.sel)
        .max((list.sel + 1).saturating_sub(height));
    if list.view.is_empty() {
        let text = if list.loading {
            "Loading…"
        } else if !list.loaded {
            "Not loaded. [R] Refresh"
        } else {
            "Nothing here. [p] Repo  [R] Refresh"
        };
        frame.render_widget(Paragraph::new(text).style(DIM).centered(), inner);
        return;
    }
    let rows: Vec<Line> = list
        .view
        .iter()
        .enumerate()
        .skip(list.top)
        .take(height)
        .map(|(n, &i)| {
            let line = columns.row(&list.items[i]);
            if n == list.sel { selected(line) } else { line }
        })
        .collect();
    frame.render_widget(Paragraph::new(rows), inner);
}

/// One solid bar; columns keep their colors.
fn selected(mut line: Line<'static>) -> Line<'static> {
    for span in &mut line.spans {
        if span.style.fg == DIM.fg {
            span.style.fg = Some(Color::Indexed(250));
        }
        span.style = span.style.add_modifier(Modifier::BOLD);
    }
    line.style(Style::new().fg(Color::Indexed(255)).bg(Color::Indexed(238)))
}

/// Column widths of the list; 0 hides a column.
struct Columns {
    state: usize,
    repo: usize,
    id: usize,
    checks: usize,
    title: usize,
    reviewers: usize,
    approval: usize,
    date: usize,
}

impl Columns {
    fn new(tab: Tab, width: usize, scoped: bool) -> Self {
        let ticket = matches!(tab, Tab::Prs | Tab::Issues);
        let pr = tab == Tab::Prs;
        let mut c = Columns {
            state: if ticket { 6 } else { 11 },
            repo: if scoped { 0 } else { (width / 6).min(22) },
            id: if ticket { 6 } else { (width / 6).min(16) },
            checks: if pr { 1 } else { 0 },
            title: 0,
            reviewers: match width {
                _ if !pr => 0,
                130.. => 30,
                100.. => 22,
                _ => 14,
            },
            approval: if pr { 10 } else { 0 },
            date: 11,
        };
        let used =
            |c: &Columns| -> usize { c.widths().iter().filter(|w| **w > 0).map(|w| w + 1).sum() };
        // Drop what matters least until the title has room.
        if width < used(&c) + 22 {
            c.date = 0;
        }
        if width < used(&c) + 18 {
            c.repo = c.repo.min(8);
            c.reviewers = c.reviewers.min(10);
        }
        c.title = width.saturating_sub(used(&c));
        c
    }

    fn widths(&self) -> [usize; 7] {
        [
            self.state,
            self.repo,
            self.id,
            self.checks,
            self.reviewers,
            self.approval,
            self.date,
        ]
    }

    fn header(&self, tab: Tab) -> Line<'static> {
        let id = match tab {
            Tab::Releases => "TAG",
            Tab::Runs => "WORKFLOW",
            _ => "#",
        };
        let mut spans = Vec::new();
        for (name, width) in [
            ("STATE", self.state),
            ("REPO", self.repo),
            (id, self.id),
            ("C", self.checks),
            ("TITLE", self.title),
            ("REVIEWERS", self.reviewers),
            ("APPROVAL", self.approval),
            ("UPDATED", self.date),
        ] {
            cell(&mut spans, name, width, Style::new());
        }
        Line::from(spans)
    }

    fn row(&self, item: &Item) -> Line<'static> {
        let state = item.state();
        let repo = item.repo().split('/').next_back().unwrap_or("");
        let (id, title) = match item {
            Item::Ticket(t) => (format!("#{}", t.number), t.title.clone()),
            Item::Release(r) => (r.tag.clone(), item.title().to_string()),
            Item::Run(r) => (
                r.name.clone().unwrap_or_default(),
                format!("{} ({})", r.title, r.branch.as_deref().unwrap_or("-")),
            ),
        };
        let reviews = match item {
            Item::Ticket(t) => t.reviews.as_ref(),
            _ => None,
        };
        let date = api::date(item.updated());
        let mut s = Vec::new();
        cell(&mut s, state, self.state, state_style(state));
        cell(&mut s, repo, self.repo, Style::new().fg(Color::Blue));
        cell(&mut s, &id, self.id, Style::new().fg(ACCENT));
        let (mark, style) = check_mark(reviews.and_then(|r| r.checks.as_deref()));
        cell(&mut s, mark, self.checks, style);
        cell(&mut s, &title, self.title, Style::new());
        if self.reviewers > 0 {
            reviewer_cell(&mut s, reviews, self.reviewers);
        }
        let approval = reviews.map_or("…", |r| r.approval());
        cell(&mut s, approval, self.approval, mark_style(approval));
        cell(&mut s, date.get(5..).unwrap_or(""), self.date, DIM);
        Line::from(s)
    }
}

/// Append a padded cell and its separator.
fn cell(spans: &mut Vec<Span<'static>>, text: &str, width: usize, style: Style) {
    if width > 0 {
        spans.push(Span::styled(fit(text, width), style));
        spans.push(Span::raw(" "));
    }
}

/// Style of a text that starts with a review mark.
fn mark_style(text: &str) -> Style {
    match text.chars().next() {
        Some('✓') => Style::new().fg(Color::Green),
        Some('✗') => Style::new().fg(Color::Red),
        Some('●') => Style::new().fg(Color::Yellow),
        _ => DIM,
    }
}

/// `✓alice ●@team +2`, each reviewer in the color of their state.
fn reviewer_cell(spans: &mut Vec<Span<'static>>, reviews: Option<&Reviews>, width: usize) {
    let Some(reviews) = reviews else {
        return cell(spans, "…", width, DIM);
    };
    let all = &reviews.reviewers;
    if all.is_empty() {
        return cell(spans, "-", width, DIM);
    }
    let mut left = width;
    for (n, r) in all.iter().enumerate() {
        let text = format!("{}{} ", r.mark(), clean(&r.name));
        let rest = all.len() - n;
        // Keep room for the "+N" counter while more reviewers follow.
        let reserve = if rest > 1 { 4 } else { 0 };
        if text.trim_end().width() + reserve > left {
            if n == 0 {
                let room = left.saturating_sub(reserve);
                spans.push(Span::styled(fit(&text, room), mark_style(&text)));
                left -= room;
                if rest > 1 {
                    spans.push(Span::styled(fit(&format!(" +{}", rest - 1), left), DIM));
                    left = 0;
                }
            } else {
                spans.push(Span::styled(fit(&format!("+{rest}"), left), DIM));
                left = 0;
            }
            break;
        }
        let len = text.width().min(left);
        spans.push(Span::styled(fit(&text, len), mark_style(&text)));
        left -= len;
    }
    spans.push(Span::raw(" ".repeat(left + 1)));
}

fn check_mark(state: Option<&str>) -> (&'static str, Style) {
    let (mark, color) = match state {
        None => return (" ", DIM),
        Some("SUCCESS") => ("✓", Color::Green),
        Some("FAILURE" | "ERROR") => ("✗", Color::Red),
        Some(_) => ("●", Color::Yellow),
    };
    (mark, Style::new().fg(color))
}

fn state_style(state: &str) -> Style {
    Style::new().fg(match state {
        "open" | "success" | "released" => Color::Green,
        "failure" | "timed_out" | "startup_failure" | "closed" => Color::Red,
        "merged" => Color::Magenta,
        "in_progress" | "queued" | "pending" | "waiting" | "pre" | "action_required" => {
            Color::Yellow
        }
        _ => Color::Gray,
    })
}

fn draw_doc(frame: &mut Frame, app: &mut App, area: Rect) {
    let name = keys::groups(app.tab)
        .iter()
        .flat_map(|g| g.keys)
        .find(|k| k.act == app.doc_act)
        .map_or("Info", |k| k.label);
    let loading = if app.doc_loading {
        " · loading…"
    } else {
        ""
    };
    let mut block = pane(app.detail_focus && app.modal.is_none())
        .title(Line::styled(format!(" {name}{loading} "), BOLD));
    block = block.title_bottom(if app.detail_focus {
        hints(&[
            ("j/k", "Scroll"),
            ("Space", "Page"),
            ("g/G", "Top/End"),
            ("Esc", "Back"),
        ])
    } else {
        hints(&[("Enter", "Open with comments/details")])
    });
    let inner = block.inner(area);
    let height = inner.height as usize;
    app.doc_height = height;
    app.doc.layout(inner.width);
    app.doc.scroll(0, height); // clamp after resize
    let percent = if app.doc.len() > height {
        let seen = (app.doc.top + height) * 100 / app.doc.len();
        format!(" {}%", seen.min(100))
    } else {
        String::new()
    };
    let mut right = Vec::new();
    if let Some((n, count, path)) = app.doc.current_file() {
        let room = (area.width as usize).saturating_sub(40);
        let path = fit(path, path.width().min(room));
        let style = Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD);
        right.push(Span::styled(format!(" {path} ({n}/{count})"), style));
    }
    right.push(Span::raw(percent));
    if right.iter().any(|s| s.width() > 0) {
        right.push(Span::raw(" "));
        block = block.title_top(Line::from(right).right_aligned());
    }
    frame.render_widget(block, area);
    for (n, row) in app.doc.visible(height).enumerate() {
        let area = Rect::new(inner.x, inner.y + n as u16, inner.width, 1);
        let (text, background) = doc_line(row.indent, row.text, row.kind);
        frame.render_widget(text, area);
        if let Some(color) = background {
            frame.buffer_mut().set_style(area, Style::new().bg(color));
        }
    }
}

/// Styled text and the background of the whole row.
fn doc_line(indent: usize, text: &str, kind: LineKind) -> (Line<'_>, Option<Color>) {
    let fg = |c| Style::new().fg(c);
    if kind.is_code() {
        let (code, background) = match kind {
            LineKind::Add => (fg(Color::Indexed(255)), Some(Color::Indexed(22))),
            LineKind::Del => (fg(Color::Indexed(255)), Some(Color::Indexed(52))),
            _ => (Style::new(), None),
        };
        let gutter = match background {
            Some(_) => fg(Color::Indexed(250)),
            None => DIM,
        };
        let line = match text.split_at_checked(GUTTER) {
            _ if indent > 0 => Line::from(vec![
                Span::raw(" ".repeat(indent)),
                Span::styled(text, code),
            ]),
            Some((numbers, rest)) => Line::from(vec![
                Span::styled(numbers, gutter),
                Span::styled(rest, code),
            ]),
            None => Line::styled(text, code),
        };
        return (line, background);
    }
    match kind {
        LineKind::Title => (Line::styled(text, BOLD), None),
        LineKind::Heading => (
            Line::styled(text, fg(ACCENT).add_modifier(Modifier::BOLD)),
            None,
        ),
        LineKind::File => (
            Line::styled(text, fg(Color::Indexed(255)).add_modifier(Modifier::BOLD)),
            Some(Color::Indexed(24)),
        ),
        LineKind::Hunk => (Line::styled(text, fg(Color::Magenta)), None),
        LineKind::Summary => (summary_line(text), None),
        _ => (Line::raw(text), None),
    }
}

/// ` M    +12     -3  path`, as written by `Doc`.
fn summary_line(text: &str) -> Line<'_> {
    let (Some(status), Some(add), Some(del), Some(path)) = (
        text.get(..3),
        text.get(3..9),
        text.get(9..16),
        text.get(16..),
    ) else {
        return Line::raw(text);
    };
    let color = match status.trim() {
        "A" => Color::Green,
        "D" => Color::Red,
        _ => Color::Yellow,
    };
    Line::from(vec![
        Span::styled(status, Style::new().fg(color).add_modifier(Modifier::BOLD)),
        Span::styled(add, Style::new().fg(Color::Green)),
        Span::styled(del, Style::new().fg(Color::Red)),
        Span::styled(path, BOLD),
    ])
}

fn group_color(group: &Group) -> Color {
    match group.name {
        "View" => Color::Blue,
        "Review" | "Edit" | "Run" => Color::Green,
        "Manage" => Color::Magenta,
        _ => Color::Gray,
    }
}

/// Put the action groups on as few rows as possible.
fn flow(app: &App, width: u16, border: usize) -> Vec<Vec<(&'static Group, u16)>> {
    let mut rows: Vec<Vec<(&'static Group, u16)>> = vec![Vec::new()];
    let mut used = 0;
    for group in keys::groups(app.tab).iter().chain([&GLOBAL]) {
        let title = if border > 0 { 0 } else { group.name.len() + 3 };
        let keys: usize = key_spans(app, group).iter().map(Span::width).sum();
        let len = keys + title + border;
        if used > 0 && used + len > width as usize {
            rows.push(Vec::new());
            used = 0;
        }
        used += len;
        if let Some(row) = rows.last_mut() {
            row.push((group, len as u16));
        }
    }
    rows
}

fn draw_actions(
    frame: &mut Frame,
    app: &App,
    area: Rect,
    rows: &[Vec<(&'static Group, u16)>],
    boxed: bool,
) {
    let height = if boxed { 3 } else { 1 };
    let mut y = area.y;
    for row in rows {
        let mut x = area.x;
        for (group, len) in row {
            let color = group_color(group);
            let place = Rect::new(x, y, *len, height).intersection(area);
            x = x.saturating_add(*len);
            if boxed {
                let block = Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(Style::new().fg(color))
                    .title(Line::styled(
                        format!(" {} ", group.name),
                        Style::new().fg(color).add_modifier(Modifier::BOLD),
                    ));
                let inner = block.inner(place);
                frame.render_widget(block, place);
                frame.render_widget(Line::from(key_spans(app, group)), inner);
            } else {
                let badge = Style::new()
                    .fg(Color::Black)
                    .bg(color)
                    .add_modifier(Modifier::BOLD);
                let mut spans = vec![
                    Span::styled(format!(" {} ", group.name), badge),
                    Span::raw(" "),
                ];
                spans.extend(key_spans(app, group));
                frame.render_widget(Line::from(spans), place);
            }
        }
        y += height;
    }
}

fn key_spans(app: &App, group: &Group) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    for k in group.keys {
        let label = match (k.act, app.list().selected().map(Item::state)) {
            (Act::Toggle, Some("open" | "draft")) => "Close",
            (Act::Toggle, Some("closed")) => "Reopen",
            _ => k.label,
        };
        // Red marks what is hard to undo.
        let risky = matches!(
            k.act,
            Act::Merge | Act::Toggle | Act::Delete | Act::Cancel | Act::Publish
        );
        let style = if risky { KEY.fg(Color::Red) } else { KEY };
        spans.push(Span::styled(format!("[{}]", k.key), style));
        spans.push(Span::raw(format!(" {label} ")));
    }
    spans
}

fn draw_status(frame: &mut Frame, app: &App, area: Rect) {
    let width = area.width as usize;
    let (text, style) = match (&app.status, &app.busy) {
        (Some(Status::Error(e)), _) => (format!(" ✗ {e}"), Style::new().fg(Color::Red)),
        (_, Some(busy)) => (format!(" … {busy}"), Style::new().fg(Color::Yellow)),
        (Some(Status::Info(i)), _) => (format!(" {i}"), Style::new().fg(Color::Green)),
        _ => return,
    };
    frame.render_widget(Line::styled(fit(&text, width), style), area);
}

fn popup(frame: &mut Frame, width: u16, height: u16, block: Block) -> Rect {
    let screen = frame.area();
    let (w, h) = (width.min(screen.width), height.min(screen.height));
    let area = Rect::new((screen.width - w) / 2, (screen.height - h) / 2, w, h);
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    inner
}

fn draw_confirm(frame: &mut Frame, label: &str) {
    let block = pane(true)
        .title(Line::styled(" Confirm ", BOLD))
        .title_bottom(hints(&[("y", "Yes"), ("n/Esc", "No")]));
    let text = format!("{label}?");
    let width = (text.width() as u16 + 6).max(40);
    let inner = popup(frame, width, 5, block);
    let line = fit(&text, (inner.width as usize).min(text.width()));
    frame.render_widget(
        Paragraph::new(vec![Line::raw(""), Line::raw(line)]).centered(),
        inner,
    );
}

fn draw_help(frame: &mut Frame, tab: Tab) {
    let mut lines: Vec<Line> = Vec::new();
    let navigation: &[(&str, &str)] = &[
        ("1-4/Tab", "Switch tab"),
        ("j/k", "Move or scroll"),
        ("g/G", "Top/End"),
        ("Space/PgUp", "Page"),
        ("^d/^u", "Half page"),
        ("Enter", "Open"),
        ("Esc", "Back"),
    ];
    let mut nav = vec![Span::styled(" NAVIGATE ", Style::new().fg(ACCENT))];
    nav.extend(hints(&navigation[..3]).spans);
    lines.push(Line::from(nav));
    let mut more = vec![Span::raw("          ")];
    more.extend(hints(&navigation[3..]).spans);
    lines.push(Line::from(more));
    for group in keys::groups(tab).iter().chain([&SCOPE, &GLOBAL]) {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            format!(" {}", group.name.to_uppercase()),
            Style::new().fg(ACCENT),
        ));
        let list: Vec<(String, &str)> = group
            .keys
            .iter()
            .map(|k| (k.key.to_string(), k.label))
            .collect();
        let list: Vec<(&str, &str)> = list.iter().map(|(k, l)| (k.as_str(), *l)).collect();
        for row in list.chunks(4) {
            lines.push(hints(row));
        }
    }
    if tab == Tab::Prs {
        lines.push(Line::raw(""));
        let marks = [
            "✓ approved",
            "✗ changes",
            "● waiting",
            "· commented",
            "- dismissed",
        ];
        let mut spans = vec![Span::styled(" REVIEWERS ", Style::new().fg(ACCENT))];
        for mark in marks {
            spans.push(Span::styled(format!(" {mark} "), mark_style(mark)));
        }
        lines.push(Line::from(spans));
    }
    let block = pane(true)
        .title(Line::styled(format!(" Help: {} ", tab.title()), BOLD))
        .title_bottom(hints(&[("any key", "Close")]));
    let inner = popup(frame, 78, lines.len() as u16 + 2, block);
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_picker(frame: &mut Frame, picker: &Picker) {
    let height = frame.area().height.saturating_sub(4).min(24);
    let multi = picker.multi();
    let keys: &[(&str, &str)] = if multi {
        &[
            ("type", "Find"),
            ("Space", "Mark"),
            ("^X", "Clear"),
            ("Enter", "Apply"),
            ("Esc", "Cancel"),
        ]
    } else {
        &[
            ("type", "Find"),
            ("↑/↓", "Move"),
            ("Enter", "Choose"),
            ("Esc", "Cancel"),
        ]
    };
    let title = match picker.marked.len() {
        0 => format!(" {} ", picker.title),
        n => format!(" {} ({n} marked) ", picker.title),
    };
    let block = pane(true)
        .title(Line::styled(title, BOLD))
        .title_bottom(hints(keys));
    let inner = popup(frame, 70, height, block);
    let width = inner.width as usize;
    let mut lines = vec![Line::from(vec![
        Span::styled("> ", KEY),
        Span::raw(picker.find.clone()),
        Span::styled("▏", KEY),
    ])];
    let rows = (inner.height as usize).saturating_sub(1);
    let top = (picker.sel + 1).saturating_sub(rows);
    for (n, item) in picker.view().into_iter().enumerate().skip(top).take(rows) {
        let style = if n == picker.sel {
            Style::new().add_modifier(Modifier::REVERSED)
        } else {
            Style::new()
        };
        let (text, style) = match (multi, picker.is_marked(&item.1)) {
            (false, _) => (item.0.clone(), style),
            (true, true) => (format!("[x] {}", item.0), style.fg(Color::Green)),
            (true, false) => (format!("[ ] {}", item.0), style),
        };
        lines.push(Line::styled(fit(&text, width), style));
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_form(frame: &mut Frame, form: &Form) {
    let heights: Vec<Constraint> = form
        .fields
        .iter()
        .map(|f| match f.kind {
            Kind::Text { multi: true, .. } => Constraint::Min(4),
            Kind::Text { .. } => Constraint::Length(3),
            _ => Constraint::Length(1),
        })
        .chain([Constraint::Length(1)])
        .collect();
    let wanted: u16 = form
        .fields
        .iter()
        .map(|f| match f.kind {
            Kind::Text { multi: true, .. } => 10,
            Kind::Text { .. } => 3,
            _ => 1,
        })
        .sum();
    let block = pane(true)
        .title(Line::styled(format!(" {} ", clean(&form.title)), BOLD))
        .title_bottom(hints(&[
            ("^S", "Submit"),
            ("Tab", "Next field"),
            ("Esc", "Cancel"),
        ]));
    let inner = popup(frame, 84, wanted + 3, block);
    let areas = Layout::vertical(heights).split(inner);
    for (i, field) in form.fields.iter().enumerate() {
        let (area, focused) = (areas[i], i == form.focus);
        let style = if focused {
            Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)
        } else {
            Style::new()
        };
        match &field.kind {
            Kind::Text { buf, cur, .. } => {
                let block = pane(focused).title(Line::styled(format!(" {} ", field.label), style));
                let text = block.inner(area);
                frame.render_widget(block, area);
                draw_text(frame, text, buf, *cur, focused);
            }
            Kind::Toggle(on) => {
                let mark = if *on { "(x) " } else { "( ) " };
                let line = Line::from(vec![
                    Span::raw(" "),
                    key("Space"),
                    Span::raw(format!(" {mark}")),
                    Span::styled(field.label, style),
                ]);
                frame.render_widget(line, area);
            }
            Kind::Choice(all, n) => {
                let mut spans = vec![
                    Span::raw(" "),
                    key("←/→"),
                    Span::raw(" "),
                    Span::styled(field.label, style),
                    Span::raw(":"),
                ];
                for (j, option) in all.iter().enumerate() {
                    let chosen = if j == *n {
                        Modifier::REVERSED
                    } else {
                        Modifier::empty()
                    };
                    spans.push(Span::raw(" "));
                    spans.push(Span::styled(
                        format!(" {option} "),
                        Style::new().add_modifier(chosen),
                    ));
                }
                frame.render_widget(Line::from(spans), area);
            }
        }
    }
    if let Some(error) = &form.error {
        let area = areas[form.fields.len()];
        let text = fit(&format!(" ✗ {error}"), area.width as usize);
        frame.render_widget(Line::styled(text, Style::new().fg(Color::Red)), area);
    }
}

/// An unwrapped text area that follows its cursor.
fn draw_text(frame: &mut Frame, area: Rect, buf: &str, cur: usize, focused: bool) {
    if area.is_empty() {
        return;
    }
    let (start, _) = line_at(buf, cur);
    let col = clean(&buf[start..cur]).width();
    let row = buf[..cur].matches('\n').count();
    let left = (col + 1).saturating_sub(area.width as usize);
    let top = (row + 1).saturating_sub(area.height as usize);
    let lines: Vec<Line> = buf
        .split('\n')
        .skip(top)
        .take(area.height as usize)
        .map(|l| Line::raw(clean(l)))
        .collect();
    frame.render_widget(Paragraph::new(lines).scroll((0, left as u16)), area);
    if focused {
        frame.set_cursor_position(Position::new(
            area.x + (col - left) as u16,
            area.y + (row - top) as u16,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use std::sync::mpsc;

    fn screen(app: &mut App, w: u16, h: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        terminal.backend().to_string()
    }

    #[test]
    fn fit_is_exact() {
        assert_eq!(fit("abc", 5), "abc  ");
        assert_eq!(fit("abcdef", 4), "abc…");
        assert_eq!(fit("日本語", 4).width(), 4);
        assert_eq!(fit("abc", 0), "");
    }

    #[test]
    fn every_shortcut_is_visible_at_80x24() {
        let mut app = App::new(mpsc::channel().0, None);
        for tab in Tab::ALL {
            app.tab = tab;
            let text = screen(&mut app, 80, 24);
            assert!(
                text.contains("Org:All") && text.contains("Repo:All"),
                "{tab:?}"
            );
            for k in keys::groups(tab)
                .iter()
                .chain([&GLOBAL])
                .flat_map(|g| g.keys)
            {
                assert!(
                    text.contains(&format!("[{}] {}", k.key, k.label)),
                    "{tab:?} {}",
                    k.label
                );
            }
        }
    }

    #[test]
    fn survives_tiny_terminals() {
        let mut app = App::new(mpsc::channel().0, None);
        for (w, h) in [(1, 1), (5, 3), (20, 6)] {
            for modal in [
                None,
                Some(Modal::Help),
                Form::new(Tab::Releases, Act::New, "o/r", None, "")
                    .map(|f| Modal::Form(Box::new(f))),
            ] {
                app.modal = modal;
                screen(&mut app, w, h);
            }
        }
    }

    #[test]
    fn header_stays_on_one_row() {
        let mut app = App::new(mpsc::channel().0, Some("some-owner/some-repository".into()));
        let choice = |v: &str| (v.to_string(), v.to_string());
        app.orgs = vec![choice("some-owner"), choice("other")];
        app.lists[0].filters = vec![choice("Opened by me"), choice("Involves me"), choice("x")];
        for (width, texts) in [
            (
                80,
                &[
                    "[1]PR",
                    "[4]Act",
                    "[o] Org:2",
                    "[p] Repo:",
                    "[f] Filter:3",
                    "[s] State:",
                ][..],
            ),
            (
                180,
                &[
                    "Pull Requests",
                    "[o] Org:some-owner +1",
                    "[p] Repo:some-owner/some-repository",
                    "[f] Filter:Opened by me +2",
                    "[s] State:Open",
                ],
            ),
        ] {
            let text = screen(&mut app, width, 24);
            let first = text.lines().next().unwrap();
            for t in texts {
                assert!(first.contains(t), "{width}: {t} missing in {first}");
            }
        }
    }

    #[test]
    fn reviewer_cell_has_exact_width() {
        use crate::api::Reviewer;
        for names in [
            &[][..],
            &["al"],
            &["a-very-long-name"],
            &["al", "bo", "a-very-long-name"],
        ] {
            let reviews = Reviews {
                reviewers: names
                    .iter()
                    .map(|n| Reviewer {
                        name: n.to_string(),
                        state: "APPROVED".into(),
                    })
                    .collect(),
                ..Reviews::default()
            };
            for width in [5, 10, 14, 30] {
                let mut spans = Vec::new();
                reviewer_cell(&mut spans, Some(&reviews), width);
                let got: usize = spans.iter().map(Span::width).sum();
                assert_eq!(got, width + 1, "{names:?}");
            }
        }
    }
}
