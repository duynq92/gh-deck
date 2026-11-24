//! Drawing. Every label that has a shortcut shows it.

use crate::activity::Event;
use crate::api::{self, Choice, Item, Reviews};
use crate::app::{App, Modal, Picker, Status};
use crate::doc::{GUTTER, Kind as LineKind, Visible, clean};
use crate::form::{Form, Kind, line_at};
use crate::keys::{self, Act, Group, Key, SCOPE, Tab};
use crate::markdown as md;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Wrap};
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
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(screen);

    draw_header(frame, app, header);
    draw_body(frame, app, body);
    frame.render_widget(footer_line(app, footer.width as usize), footer);

    match &app.modal {
        Some(Modal::Help) => draw_help(frame, app.tab),
        Some(Modal::Menu(sel)) => draw_menu(frame, app, *sel),
        Some(Modal::Message(text)) => draw_message(frame, text),
        Some(Modal::Confirm(cmd)) => draw_confirm(frame, &cmd.label),
        Some(Modal::Form(form)) => draw_form(frame, form),
        Some(Modal::Picker(p)) => draw_picker(frame, p),
        None => {}
    }
}

fn draw_body(frame: &mut Frame, app: &mut App, body: Rect) {
    if app.zoom && app.detail_focus {
        return draw_doc(frame, app, body);
    }
    let split = if body.width >= 110 {
        // Reading the content matters most.
        Layout::horizontal([Constraint::Ratio(1, 3), Constraint::Fill(1)])
    } else if body.height >= 24 {
        // Stacked panes give columns and code the full width.
        let list = if app.detail_focus { 25 } else { 55 };
        Layout::vertical([Constraint::Percentage(list), Constraint::Fill(1)])
    } else if app.detail_focus {
        return draw_doc(frame, app, body);
    } else {
        return draw_lists(frame, app, body);
    };
    let [list, doc] = split.areas(body);
    draw_lists(frame, app, list);
    draw_doc(frame, app, doc);
}

/// The activity is below the pull requests, or instead of them in a low area.
fn draw_lists(frame: &mut Frame, app: &mut App, area: Rect) {
    if app.tab.bar() != Tab::Prs || area.height < 16 {
        return draw_list(frame, app, app.tab, area);
    }
    let [prs, activity] =
        Layout::vertical([Constraint::Fill(1), Constraint::Percentage(40)]).areas(area);
    draw_list(frame, app, Tab::Prs, prs);
    draw_list(frame, app, Tab::Activity, activity);
}

#[derive(Clone, Copy)]
enum TabSize {
    Full,
    Short,
    Tiny,
    /// Only the keys of the other tabs.
    Keys,
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
    let shown = app.tab.bar();
    let list = &app.lists[shown as usize];
    let values: Vec<(&str, &[Choice], char)> = SCOPE
        .keys
        .iter()
        .filter_map(|k| {
            let chosen = match k.act {
                Act::Org => &app.orgs,
                Act::Repo => &app.scope,
                Act::Filter if api::filters(shown, &[]).is_empty() => return None,
                Act::Filter => &list.filters,
                _ if api::states(shown).is_empty() => return None,
                _ => &list.states,
            };
            Some((k.label, chosen.as_slice(), k.key))
        })
        .collect();
    let tabs = |size: TabSize| {
        let mut spans = Vec::new();
        for (i, tab) in Tab::BAR.into_iter().enumerate() {
            let new = match (tab, app.unread.len()) {
                (Tab::Prs, n) if n > 0 => format!("●{n} "),
                _ => String::new(),
            };
            let name = match size {
                TabSize::Full => format!(" {} {new}", tab.title()),
                TabSize::Short => format!(" {} {new}", tab.short()),
                TabSize::Keys if tab != shown && !new.is_empty() => new,
                TabSize::Keys if tab != shown => " ".to_string(),
                TabSize::Tiny | TabSize::Keys => format!("{} {new}", tab.tiny()),
            };
            let style = if tab == shown {
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
        (TabSize::Keys, "", 1),
    ];
    let message = status(app);
    // Long messages are cut; a readable part must stay in the center.
    let room = message.as_ref().map_or(0, |m| m.0.width().min(40) + 2);
    let mut best = (tabs(TabSize::Tiny), right(" ", 1));
    'fit: for (size, gap, min) in sizes {
        for longest in (min..=40).rev() {
            let (l, r) = (tabs(size), right(gap, longest));
            let fits = l.width() + r.width() + room < width;
            best = (l, r);
            if fits {
                break 'fit;
            }
        }
    }
    let (left, right) = best;
    let len = (right.width() as u16).min(area.width);
    if let Some((text, style)) = message {
        let from = area.x + left.width() as u16 + 1;
        let to = (area.right() - len).saturating_sub(1);
        let width = (text.width() as u16).min(to.saturating_sub(from));
        // The middle of the screen, or as close as the neighbors allow.
        let x = (area.x + area.width.saturating_sub(width) / 2)
            .min(to.saturating_sub(width))
            .max(from);
        let place = Rect::new(x, area.y, width, 1).intersection(area);
        frame.render_widget(Line::styled(fit(&text, place.width as usize), style), place);
    }
    frame.render_widget(left, area);
    let area = Rect::new(area.right() - len, area.y, len, area.height);
    frame.render_widget(right, area);
}

fn draw_list(frame: &mut Frame, app: &mut App, tab: Tab, area: Rect) {
    let active = tab == app.tab;
    let finding = app.finding && active;
    let mut block = pane(active && !app.detail_focus && app.modal.is_none());
    let list = &app.lists[tab as usize];
    let count = if tab == Tab::Activity && list.find.is_empty() {
        list.view.len().to_string() // without hidden bots
    } else if !list.find.is_empty() {
        format!("{}/{}", list.view.len(), list.items.len())
    } else if list.cursor.is_some() || list.total > list.items.len() {
        format!("{} of {}", list.items.len(), list.total)
    } else {
        list.items.len().to_string()
    };
    let loading = if list.loading { " · loading…" } else { "" };
    let mut title = vec![Span::raw(" ")];
    if !active {
        title.push(key(&(tab as usize + 1).to_string()));
        title.push(Span::raw(" "));
    }
    let name = format!("{} ({count})", tab.title());
    title.push(Span::styled(name, BOLD));
    if tab == Tab::Activity && !app.unread.is_empty() {
        title.push(Span::styled(format!(" ●{}", app.unread.len()), KEY));
    }
    title.push(Span::raw(format!("{loading} ")));
    block = block.title(Line::from(title));
    if finding || !list.find.is_empty() {
        let mut spans = vec![Span::raw(" "), key("/"), Span::raw(" Find: ")];
        spans.push(Span::styled(clean(&list.find).into_owned(), BOLD));
        if finding {
            spans.push(Span::styled("▏", KEY));
            spans.extend(hints(&[("Enter", "Keep"), ("Esc", "Clear")]).spans);
        }
        spans.push(Span::raw(" "));
        block = block.title_top(Line::from(spans).right_aligned());
    }
    if active {
        let mut keys = vec![("j/k", "Move"), ("Enter", "Open")];
        match tab {
            Tab::Prs => keys.push(("5", "Activity")),
            Tab::Activity => keys.push(("Esc", "Pull requests")),
            _ => keys.push(("g/G", "Top/End")),
        }
        block = block.title_bottom(hints(&keys));
    }
    let mut inner = block.inner(area);
    frame.render_widget(block, area);
    let columns = Columns::new(tab, inner.width as usize, app.scope.len() == 1);
    if inner.height > 3 {
        let header = Rect { height: 1, ..inner };
        frame.render_widget(columns.header(tab).style(HEAD), header);
        inner.y += 1;
        inner.height -= 1;
    }
    let height = inner.height as usize;
    if active {
        app.list_height = height;
    }

    let list = &mut app.lists[tab as usize];
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
            let unread = match &list.items[i] {
                Item::Event(e) => app.unread.contains(&e.key),
                item => tab == Tab::Prs && app.unread_prs.contains(&item.id()),
            };
            let line = columns.row(&list.items[i], unread);
            let here = active && n == list.sel;
            if here { selected(line) } else { line }
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
        if tab == Tab::Prs {
            return Columns::pull_requests(width, scoped);
        }
        if tab == Tab::Activity {
            let mut c = Columns {
                state: 1,
                repo: if scoped { 0 } else { (width / 7).min(14) },
                id: 6,
                checks: 0,
                title: 0,
                reviewers: 0,
                approval: 0,
                date: if width >= 60 { 11 } else { 0 },
            };
            c.title = width.saturating_sub(c.used());
            return c;
        }
        let wide = width >= 70;
        let mut c = Columns {
            state: match (wide, ticket) {
                (false, _) => 1,
                (_, true) => 6,
                _ => 11,
            },
            repo: if scoped { 0 } else { (width / 6).min(22) },
            id: if ticket { 6 } else { (width / 6).clamp(8, 16) },
            checks: 0,
            title: 0,
            reviewers: 0,
            approval: 0,
            date: 11,
        };
        // Drop what matters least until the title has room.
        if width < c.used() + 22 {
            c.date = 0;
        }
        if width < c.used() + 18 {
            c.repo = c.repo.min(8);
        }
        c.title = width.saturating_sub(c.used());
        c
    }

    /// Narrow lists show the state as a letter and the approval as a mark.
    fn pull_requests(width: usize, scoped: bool) -> Self {
        let wide = width >= 90;
        let mut c = Columns {
            state: if wide { 6 } else { 1 },
            repo: match (scoped, wide) {
                (true, _) => 0,
                (_, true) => (width / 9).min(14),
                _ => (width / 6).clamp(6, 10),
            },
            id: 6,
            checks: 1,
            title: 0,
            reviewers: match width {
                130.. => 30,
                100.. => 22,
                90.. => 14,
                60.. => 12,
                _ => 8,
            },
            approval: if wide { 10 } else { 1 },
            date: 0,
        };
        if width < c.used() + 14 {
            c.reviewers = 0; // the info panel names them
        }
        c.title = width.saturating_sub(c.used());
        c
    }

    /// Columns taken by everything but the title, with separators.
    fn used(&self) -> usize {
        let widths = self.widths();
        let shown = widths.iter().filter(|w| **w > 0);
        shown.map(|w| w + 1).sum::<usize>()
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
            (
                match self.state {
                    _ if tab == Tab::Activity => "",
                    1 => "S",
                    _ => "STATE",
                },
                self.state,
            ),
            ("REPO", self.repo),
            (id, self.id),
            ("C", self.checks),
            (
                if tab == Tab::Activity {
                    "WHAT HAPPENED"
                } else {
                    "TITLE"
                },
                self.title,
            ),
            ("REVIEWERS", self.reviewers),
            (
                if self.approval > 1 { "APPROVAL" } else { "A" },
                self.approval,
            ),
            ("UPDATED", self.date),
        ] {
            cell(&mut spans, name, width, Style::new());
        }
        Line::from(spans)
    }

    fn event(&self, e: &Event, unread: bool) -> Line<'static> {
        let number = match &*e.pr {
            Item::Ticket(t) => format!("#{}", t.number),
            _ => String::new(),
        };
        let repo = e.pr.repo().split('/').next_back().unwrap_or("");
        let style = match unread {
            true => mark_style(&e.text).add_modifier(Modifier::BOLD),
            false => DIM,
        };
        let date = api::date(&e.at);
        let mut s = Vec::new();
        cell(&mut s, if unread { "●" } else { " " }, self.state, KEY);
        cell(&mut s, repo, self.repo, Style::new().fg(Color::Blue));
        cell(&mut s, &number, self.id, Style::new().fg(ACCENT));
        cell(&mut s, &e.text, self.title, style);
        cell(&mut s, date.get(5..).unwrap_or(""), self.date, DIM);
        Line::from(s)
    }

    fn row(&self, item: &Item, unread: bool) -> Line<'static> {
        if let Item::Event(e) = item {
            return self.event(e, unread);
        }
        let state = item.state();
        let repo = item.repo().split('/').next_back().unwrap_or("");
        let (id, title) = match item {
            Item::Ticket(t) => (format!("#{}", t.number), t.title.clone()),
            Item::Release(r) => (r.tag.clone(), item.title().to_string()),
            Item::Event(e) => (String::new(), e.text.clone()),
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
        let letter = match item {
            Item::Run(_) => run_mark(state).to_string(),
            _ => state.get(..1).unwrap_or("").to_uppercase(),
        };
        let name = if self.state > 1 { state } else { &letter };
        cell(&mut s, name, self.state, state_style(state));
        cell(&mut s, repo, self.repo, Style::new().fg(Color::Blue));
        cell(&mut s, &id, self.id, Style::new().fg(ACCENT));
        let (mark, style) = check_mark(reviews.and_then(|r| r.checks.as_deref()));
        cell(&mut s, mark, self.checks, style);
        // Something new since it was opened last.
        if unread && self.title > 2 {
            s.push(Span::styled("● ", KEY));
            cell(&mut s, &title, self.title - 2, BOLD);
        } else {
            cell(&mut s, &title, self.title, Style::new());
        }
        if self.reviewers > 0 {
            reviewer_cell(&mut s, reviews, self.reviewers);
        }
        let approval = reviews.map_or("…", |r| r.approval());
        let style = mark_style(approval);
        let mark = approval.get(..approval.chars().next().map_or(0, char::len_utf8));
        let text = if self.approval > 1 {
            approval
        } else {
            mark.unwrap_or("")
        };
        cell(&mut s, text, self.approval, style);
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
        Some('◆' | '@') => Style::new().fg(Color::Magenta),
        Some('↑' | '⇡' | '○') => Style::new().fg(Color::Cyan),
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
    if width <= 8 && all.len() > 1 {
        // No room for names: count them by state.
        let mut left = width;
        for mark in ['✗', '✓', '●', '·', '-'] {
            let count = all.iter().filter(|r| r.mark() == mark).count();
            let text = format!("{mark}{count} ");
            if count > 0 && text.width() - 1 <= left {
                let len = text.width().min(left);
                spans.push(Span::styled(fit(text.trim_end(), len), mark_style(&text)));
                left -= len;
            }
        }
        spans.push(Span::raw(" ".repeat(left + 1)));
        return;
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

/// State of a run where there is no room for its name.
fn run_mark(state: &str) -> &'static str {
    match state {
        "success" => "✓",
        "failure" | "timed_out" | "startup_failure" => "✗",
        "in_progress" | "queued" | "pending" | "waiting" | "requested" | "action_required" => "●",
        _ => "-",
    }
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
    let name = if app.raw && app.applies(Act::Raw) {
        format!("{name} (raw)")
    } else {
        name.to_string()
    };
    let mut block = pane(app.detail_focus && app.modal.is_none())
        .title(Line::styled(format!(" {name}{loading} "), BOLD));
    let line_mode = app.detail_focus && app.doc.cursor.is_some();
    block = block.title_bottom(if line_mode {
        hints(&[
            ("j/k", "Line"),
            ("n/N", "File"),
            ("u/U", "Discussion"),
            ("Esc", "Leave line mode"),
        ])
    } else if app.detail_focus {
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
        let (text, background) = doc_line(&row);
        frame.render_widget(text, area);
        if let Some(color) = background {
            frame.buffer_mut().set_style(area, Style::new().bg(color));
        }
        if row.selected {
            let style = Style::new().add_modifier(Modifier::REVERSED);
            frame.buffer_mut().set_style(area, style);
        }
    }
}

/// Styled text and the background of the whole row.
fn doc_line<'a>(row: &Visible<'a>) -> (Line<'a>, Option<Color>) {
    // Only rendered Markdown has more than one part.
    let (kind, indent) = (row.kind, row.indent);
    let text = row.parts.first().map_or("", |p| p.0);
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
    let (base, background) = match kind {
        LineKind::Title => (BOLD, None),
        LineKind::Heading => (fg(ACCENT).add_modifier(Modifier::BOLD), None),
        LineKind::File => (
            fg(Color::Indexed(255)).add_modifier(Modifier::BOLD),
            Some(Color::Indexed(24)),
        ),
        LineKind::Hunk => (fg(Color::Magenta), None),
        LineKind::Block => (fg(Color::Indexed(252)), Some(Color::Indexed(236))),
        LineKind::Open => (
            fg(Color::Indexed(220)).add_modifier(Modifier::BOLD),
            Some(Color::Indexed(235)),
        ),
        LineKind::Finding => (
            fg(Color::Indexed(177)).add_modifier(Modifier::BOLD),
            Some(Color::Indexed(235)),
        ),
        LineKind::Accepted => (
            fg(Color::Indexed(114)).add_modifier(Modifier::BOLD),
            Some(Color::Indexed(235)),
        ),
        LineKind::Done => (fg(Color::Indexed(114)), Some(Color::Indexed(235))),
        LineKind::Note => (fg(Color::Indexed(252)), Some(Color::Indexed(235))),
        LineKind::Summary if indent == 0 => return (summary_line(text), None),
        _ => (Style::new(), None),
    };
    let mut margin = " ".repeat(indent);
    if kind == LineKind::Note && indent > GUTTER {
        margin = format!("{}┃{}", " ".repeat(GUTTER), " ".repeat(indent - GUTTER - 1));
    }
    let mut spans = vec![Span::raw(margin)];
    let parts = row.parts.iter();
    spans.extend(parts.map(|(text, bits)| Span::styled(*text, base.patch(inline(*bits)))));
    (Line::from(spans), background)
}

/// Style of rendered Markdown.
fn inline(bits: u8) -> Style {
    let mut style = Style::new();
    for (bit, part) in [
        (md::BOLD, BOLD),
        (md::ITALIC, Style::new().add_modifier(Modifier::ITALIC)),
        (md::STRIKE, Style::new().add_modifier(Modifier::CROSSED_OUT)),
        (
            md::LINK,
            Style::new()
                .fg(Color::Blue)
                .add_modifier(Modifier::UNDERLINED),
        ),
        (
            md::CODE,
            Style::new().fg(Color::Indexed(222)).bg(Color::Indexed(236)),
        ),
        (md::DIM, DIM),
    ] {
        if bits & bit != 0 {
            style = style.patch(part);
        }
    }
    style
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
        "My PR" => Color::Yellow,
        "Teams" => Color::Cyan,
        "AI" => Color::Indexed(177),
        "Activity" => Color::Green,
        _ => Color::Gray,
    }
}

/// `[n/N] Next/prev file `, with the label that fits the selected item.
fn action(app: &App, name: &str, k: &Key, color: Color) -> [Span<'static>; 2] {
    let label = match (k.act, app.current().map(Item::state)) {
        (Act::Toggle, Some("open" | "draft")) => "Close",
        (Act::Toggle, Some("closed")) => "Reopen",
        (Act::Raw, _) if app.raw => "Rendered",
        (Act::Bots, _) if app.list().bots => "Hide bots",
        _ => k.label,
    };
    let count = match (k.act, app.excluded.len()) {
        (Act::Exclude, n) if n > 0 => format!(" ({n})"),
        _ => String::new(),
    };
    // Red marks what is hard to undo.
    let risky = matches!(
        k.act,
        Act::Merge | Act::Toggle | Act::Delete | Act::Cancel | Act::Publish
    );
    let color = if risky { Color::Red } else { color };
    let style = Style::new().fg(color).add_modifier(Modifier::BOLD);
    [
        Span::styled(format!("[{name}]"), style),
        Span::raw(format!(" {label}{count} ")),
    ]
}

/// What is useful right now, as far as it fits; the popup has the rest.
fn footer_line(app: &App, width: usize) -> Line<'static> {
    if app.detail_focus && app.doc.cursor.is_some() {
        let keys: &[(&str, &str)] = match (app.doc.thread_at_cursor(), app.doc.anchor) {
            _ if app.doc.finding_at_cursor().is_some() => &[
                ("Space", "Accept"),
                ("e", "Edit"),
                ("d", "Discard"),
                ("P", "Post accepted"),
                ("u/U", "Next"),
            ],
            (Some(_), _) => &[
                ("Enter", "Reply"),
                ("x", "Resolve/Reopen"),
                ("O", "Resolved"),
            ],
            (None, None) => &[
                ("Enter", "Comment"),
                ("Space", "Start range"),
                ("z", "Zoom"),
            ],
            (None, Some(_)) => &[("Enter", "Comment on range"), ("Space", "Drop range")],
        };
        let mut line = hints(keys);
        let badge = Style::new().fg(Color::Black).bg(Color::Yellow);
        line.spans.insert(
            0,
            Span::styled(" LINE MODE ", badge.add_modifier(Modifier::BOLD)),
        );
        line.spans.extend(hints(&[("Esc", "Leave")]).spans);
        return line;
    }
    let find = |act: Act| {
        keys::all(app.tab).find_map(|g| {
            let (name, k) = g.shown().find(|(_, k)| k.act == act)?;
            Some(action(app, &name, k, group_color(g)))
        })
    };
    let len = |spans: &[Span]| spans.iter().map(Span::width).sum::<usize>();
    let end: Vec<Span> = [Act::Menu, Act::Help]
        .into_iter()
        .filter_map(find)
        .flatten()
        .collect();
    let mut spans = vec![Span::raw(" ")];
    let acts = keys::footer(app.tab, app.detail_focus).iter().copied();
    for part in acts.filter(|a| app.applies(*a)).filter_map(find) {
        if len(&spans) + len(&part) + len(&end) > width {
            break;
        }
        spans.extend(part);
    }
    let gap = width.saturating_sub(len(&spans) + len(&end));
    spans.push(Span::raw(" ".repeat(gap)));
    spans.extend(end);
    Line::from(spans)
}

fn draw_menu(frame: &mut Frame, app: &App, sel: usize) {
    let chosen = keys::menu(app.tab).get(sel).map(|i| i.1.act);
    let mut lines = Vec::new();
    for group in keys::all(app.tab) {
        let color = group_color(group);
        let badge = Style::new()
            .fg(Color::Black)
            .bg(color)
            .add_modifier(Modifier::BOLD);
        let mut spans = vec![
            Span::styled(format!(" {:<7} ", group.name), badge),
            Span::raw(" "),
        ];
        for (name, k) in group.shown().filter(|(_, k)| k.act != Act::Menu) {
            let here =
                chosen == Some(k.act) || (chosen == Some(Act::PrevFile) && k.act == Act::NextFile);
            for mut span in action(app, &name, k, color) {
                if !app.applies(k.act) {
                    span.style = DIM;
                }
                if here {
                    span.style = span.style.add_modifier(Modifier::REVERSED);
                }
                spans.push(span);
            }
        }
        lines.push(Line::raw(""));
        lines.push(Line::from(spans));
    }
    let item = app.current().map(Item::id).unwrap_or_default();
    let block = pane(true)
        .title(Line::styled(format!(" Actions {} ", clean(&item)), BOLD))
        .title_bottom(hints(&[
            ("letter", "Run"),
            ("←↑↓→", "Move"),
            ("Enter", "Run selected"),
            ("Esc", "Close"),
        ]));
    let width = lines.iter().map(Line::width).max().unwrap_or(0) as u16 + 2;
    let inner = popup(frame, width.max(60), lines.len() as u16 + 3, block);
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Text and style of the current message.
fn status(app: &App) -> Option<(String, Style)> {
    let (text, color) = match (&app.status, &app.busy) {
        (Some(Status::Error(e)), _) => (format!("✗ {e}"), Color::Red),
        (_, Some(busy)) => (format!("… {busy}"), Color::Yellow),
        (Some(Status::Info(i)), _) => (i.clone(), Color::Green),
        _ => (format!("… {}", app.ai_running.as_ref()?), Color::Yellow),
    };
    let text = clean(&text).replace('\n', " ");
    Some((text, Style::new().fg(color).add_modifier(Modifier::BOLD)))
}

fn draw_message(frame: &mut Frame, text: &str) {
    let block = pane(true)
        .title(Line::styled(" Message ", BOLD))
        .title_bottom(hints(&[("any key", "Close")]));
    let text = clean(text);
    // Word wrapping may need a little more than the exact number of rows.
    let height: usize = text.lines().map(|l| l.width() / 62 + 1).sum();
    let height = u16::try_from(height).unwrap_or(u16::MAX).saturating_add(2);
    let text = Paragraph::new(text).wrap(Wrap { trim: true });
    let inner = popup(frame, 76, height, block);
    frame.render_widget(text, inner);
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
    let text = format!("{}?", clean(label));
    let len = u16::try_from(text.width()).unwrap_or(u16::MAX);
    let width = len.saturating_add(6).clamp(40, 76);
    // Word wrapping may need a little more than the exact number of rows.
    let rows = len / (width - 8) + 1;
    let inner = popup(frame, width, rows + 4, block);
    let inner = Rect {
        y: inner.y + 1.min(inner.height),
        height: inner.height.saturating_sub(1),
        ..inner
    };
    let text = Paragraph::new(text).wrap(Wrap { trim: true }).centered();
    frame.render_widget(text, inner);
}

fn draw_help(frame: &mut Frame, tab: Tab) {
    let mut lines: Vec<Line> = Vec::new();
    let navigation: &[(&str, &str)] = &[
        ("1-4/Tab", "Tab, 5 Activity"),
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
    for group in keys::groups(tab).iter().chain([&SCOPE, &keys::GLOBAL]) {
        lines.push(Line::raw(""));
        lines.push(Line::styled(
            format!(" {}", group.name.to_uppercase()),
            Style::new().fg(ACCENT),
        ));
        let list: Vec<(String, &str)> = group.shown().map(|(n, k)| (n, k.label)).collect();
        let list: Vec<(&str, &str)> = list.iter().map(|(k, l)| (k.as_str(), *l)).collect();
        for row in list.chunks(4) {
            lines.push(hints(row));
        }
    }
    if matches!(tab, Tab::Prs | Tab::Activity) {
        lines.push(Line::raw(
            " AI REVIEW  A start, Space accept, e edit, d discard, P post accepted",
        ));
        lines.push(Line::raw(
            " LINE MODE  l start, Space range, Enter comment or reply, x resolve",
        ));
        let columns = " COLUMNS  S state: Open, Draft, Merged, Closed   C checks   A approval";
        lines.push(Line::raw(columns));
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
    // Descriptions are long: use most of the screen.
    let (width, wanted) = match form.act {
        Act::Description => (120, 40),
        _ => (84, wanted),
    };
    let notes = form.notes.len() as u16;
    let mut inner = popup(frame, width, wanted + notes + 3, block);
    for (good, text) in &form.notes {
        let (mark, color) = if *good {
            ("✓", Color::Green)
        } else {
            ("✗", Color::Red)
        };
        let row = Rect {
            height: 1.min(inner.height),
            ..inner
        };
        let text = fit(&format!(" {mark} {text}"), row.width as usize);
        frame.render_widget(Line::styled(text, Style::new().fg(color)), row);
        inner.y += row.height;
        inner.height -= row.height;
    }
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
    fn popup_has_every_shortcut_at_80x24() {
        let mut app = App::new(mpsc::channel().0, None);
        for tab in Tab::ALL {
            app.tab = tab;
            let text = screen(&mut app, 80, 24);
            for part in ["Org:All", "Repo:All", "[.] All actions", "[?] Help"] {
                assert!(text.contains(part), "{tab:?} {part}");
            }
            app.modal = Some(Modal::Menu(0));
            let text = screen(&mut app, 80, 24);
            app.modal = None;
            for (name, k) in keys::all(tab).flat_map(Group::shown) {
                let shown = text.contains(&format!("[{name}] {}", k.label));
                assert!(shown, "{tab:?} {}", k.label);
            }
        }
    }

    #[test]
    fn survives_tiny_terminals() {
        let mut app = App::new(mpsc::channel().0, None);
        app.status = Some(Status::Error("no room".into()));
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
                    "[4]",
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
            for width in [5, 8, 10, 14, 30] {
                let mut spans = Vec::new();
                reviewer_cell(&mut spans, Some(&reviews), width);
                let got: usize = spans.iter().map(Span::width).sum();
                assert_eq!(got, width + 1, "{names:?}");
            }
        }
    }

    #[test]
    fn message_is_centered_in_the_header() {
        let mut app = App::new(mpsc::channel().0, Some("o/r".into()));
        app.status = Some(Status::Info("saved".into()));
        let text = screen(&mut app, 180, 24);
        let first: Vec<char> = text.lines().next().unwrap().chars().collect();
        // TestBackend quotes each row.
        let middle: String = first[1 + 87..1 + 92].iter().collect();
        assert_eq!(middle, "saved");
        assert_eq!(text.matches("saved").count(), 1);

        app.status = Some(Status::Error("something went very wrong ".repeat(9)));
        for width in [80, 100, 150] {
            let text = screen(&mut app, width, 24);
            let first = text.lines().next().unwrap();
            for part in [
                "✗ somet",
                "[1]",
                "[4]",
                "Org:",
                "Repo:",
                "Filter:",
                "State:",
            ] {
                assert!(first.contains(part), "{width}: {part} missing in {first}");
            }
        }
    }

    #[test]
    fn every_tab_gives_the_list_a_third() {
        let mut app = App::new(mpsc::channel().0, None);
        for width in [110, 150, 190] {
            let mut edges = Vec::new();
            for tab in Tab::ALL {
                app.tab = tab;
                let text = screen(&mut app, width, 30);
                let row: Vec<char> = text.lines().nth(1).unwrap().chars().collect();
                edges.push(row.iter().position(|c| *c == '╮').unwrap());
            }
            assert!(edges.iter().all(|e| *e == edges[0]), "{width}: {edges:?}");
            assert_eq!(edges[0], (width as usize + 1) / 3, "{width}");
        }
    }

    #[test]
    fn activity_is_below_the_pull_requests() {
        let mut app = App::new(mpsc::channel().0, None);
        for (w, h) in [(80, 24), (83, 57), (150, 40)] {
            for (tab, other) in [
                (Tab::Prs, "[5] Activity (0)"),
                (Tab::Activity, "[1] Pull Requests (0)"),
            ] {
                app.tab = tab;
                let text = screen(&mut app, w, h);
                let row = |s: &str| text.lines().position(|l| l.contains(s));
                assert!(row(other).is_some(), "{w}x{h} {other}\n{text}");
                assert!(row("Pull Requests (0)") < row("Activity (0)"));
                let first = text.lines().next().unwrap();
                assert!(first.contains("State:") && !first.contains("[5]"));
            }
            app.tab = Tab::Issues;
            assert!(!screen(&mut app, w, h).contains("Activity ("));
        }
    }

    #[test]
    fn footer_fits_and_hides_what_does_not_apply() {
        let mut app = App::new(mpsc::channel().0, None);
        for tab in Tab::ALL {
            app.tab = tab;
            for reading in [false, true] {
                app.detail_focus = reading;
                for width in 40..200 {
                    assert!(footer_line(&app, width).width() <= width, "{tab:?} {width}");
                }
                // Nothing is selected.
                let text = footer_line(&app, 200).to_string();
                assert!(
                    !text.contains("Merge") && !text.contains("Delete"),
                    "{text}"
                );
            }
        }
    }

    /// Random keys and messages, drawn at random sizes. Tests cannot run `gh`.
    #[test]
    fn random_use_never_panics() {
        use crate::api::{Candidates, Page, Readiness, Reviewer, Run, Ticket};
        use crate::app::Msg;
        use crate::doc::Source;
        use crate::review::{Comment, Thread};
        use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

        let text = "é • **日本** `x`\n\n- ```\n  c\n  ```\n| a | b |\n|---|---|\n| 😀 | ệ |\n";
        let page = |count: u64, kind: u32| Page {
            total: 99,
            cursor: (count.is_multiple_of(2)).then(|| "c".to_string()),
            items: (0..count)
                .map(|n| match kind {
                    0 => Item::Ticket(Ticket {
                        id: format!("id{n}"),
                        repo: "ö/日本".into(),
                        number: n,
                        title: text.into(),
                        state: ["OPEN", "MERGED", "CLOSED"][n as usize % 3].into(),
                        author: Some(["me", "al"][n as usize % 2].into()),
                        body: text.into(),
                        head: "h".into(),
                        draft: n.is_multiple_of(4),
                        updated: "é".into(),
                        ..Ticket::default()
                    }),
                    3 => Item::Event(crate::activity::Event {
                        key: format!("k{n}"),
                        text: text.into(),
                        at: "é".into(),
                        bot: n % 3 == 0,
                        comment: Some(1),
                        pr: Box::new(Item::Ticket(Ticket {
                            repo: "ö/日本".into(),
                            number: n % 4,
                            head: "h".into(),
                            body: text.into(),
                            state: "OPEN".into(),
                            ..Ticket::default()
                        })),
                    }),
                    1 => Item::Release(crate::api::Release {
                        repo: "o/r".into(),
                        tag: text.into(),
                        body: Some(text.into()),
                        draft: n.is_multiple_of(2),
                        ..Default::default()
                    }),
                    _ => Item::Run(Run {
                        repo: "o/r".into(),
                        id: n,
                        title: text.into(),
                        ..Run::default()
                    }),
                })
                .collect(),
            warning: None,
        };
        let source = |n: usize| Source {
            head: format!("T\n{text}"),
            body: format!("── Description\n{text}"),
            tail: format!("── @a\n{text}"),
            diff: Some(format!(
                "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,2 +1,{} @@\n-é\n 日\n{}",
                n + 1,
                "+ệ long line of code\n".repeat(n)
            )),
            markdown: true,
            oid: "x".into(),
            threads: (0..n % 4)
                .map(|t| Thread {
                    id: t.to_string(),
                    path: ["f", "other"][t % 2].into(),
                    line: (t != 1).then_some(t as u32 + 1),
                    side: ["RIGHT", "LEFT"][t % 2].into(),
                    resolved: t == 2,
                    can_reply: true,
                    can_toggle: true,
                    total: 60,
                    comments: vec![Comment {
                        id: Some(1),
                        body: text.into(),
                        at: "é".into(),
                        ..Comment::default()
                    }],
                    ..Thread::default()
                })
                .collect(),
            show_resolved: n.is_multiple_of(2),
            ai: None,
        };
        let mut seed = 11u32;
        let mut next = move |below: u32| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 12) % below
        };
        for round in 0..4 {
            let mut app = App::new(
                mpsc::channel().0,
                (round % 2 == 0).then(|| "o/r".to_string()),
            );
            app.handle(Msg::Repos(Ok((
                crate::api::Repos {
                    login: "me".into(),
                    orgs: vec!["o".into()],
                    names: vec!["o/r".into(), "o/s".into()],
                    ..Default::default()
                },
                None,
            ))));
            app.handle(Msg::Teams(Ok(vec!["o/t".into(), "o/é".into()])));
            for step in 0..1_000 {
                match next(42) {
                    40 => app.feed_ai(text, next(6) as usize),
                    0 => app.feed(Tab::Prs, next(2) == 0, Ok(page(next(30).into(), 0))),
                    1 => app.feed(Tab::Issues, false, Ok(page(next(9).into(), 0))),
                    2 => app.feed(Tab::Releases, false, Ok(page(next(9).into(), 1))),
                    3 => app.feed(Tab::Runs, false, Ok(page(next(9).into(), 2))),
                    41 => app.feed(Tab::Activity, false, Ok(page(next(20).into(), 3))),
                    4 => app.feed_doc(next(2) == 0, Ok(source(next(60) as usize))),
                    5 => app.feed_doc(next(2) == 0, Err(crate::gh::GhError(text.into()))),
                    6 => app.handle(Msg::Reviews(Ok(vec![Reviews {
                        id: format!("id{}", next(9)),
                        reviewers: (0..next(6))
                            .map(|n| Reviewer {
                                name: format!("日本é{n}"),
                                state: ["APPROVED", "REQUESTED", "X"][n as usize % 3].into(),
                            })
                            .collect(),
                        ..Reviews::default()
                    }]))),
                    7 => app.feed_pick(
                        Candidates {
                            mine: true,
                            requested: vec!["o/t".into()],
                            people: vec![("é".into(), "al".into())],
                        },
                        text,
                        Readiness {
                            methods: vec!["Squash"],
                            notes: vec![(false, text.into())],
                            ..Readiness::default()
                        },
                        next(3),
                    ),
                    n => {
                        let code = match next(26) {
                            0 => KeyCode::Enter,
                            1 | 2 => KeyCode::Esc,
                            3 => KeyCode::Up,
                            4 => KeyCode::Down,
                            5 => KeyCode::Left,
                            6 => KeyCode::Right,
                            7 => KeyCode::Tab,
                            8 => KeyCode::Backspace,
                            9 => KeyCode::PageDown,
                            10 => KeyCode::PageUp,
                            11 => KeyCode::Char('ệ'),
                            12 => KeyCode::Char(' '),
                            _ => KeyCode::Char(char::from(33 + next(94) as u8)),
                        };
                        // Ctrl+S submits; Ctrl+C and q would end the round.
                        let key = match n {
                            8 => KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL),
                            9 => KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL),
                            _ => KeyEvent::from(code),
                        };
                        app.handle(Msg::Input(Ok(Event::Key(key))));
                        app.quit = false;
                        if n == 10 {
                            app.handle(Msg::Input(Ok(Event::Paste(text.into()))));
                        }
                        if n == 11 {
                            app.busy = None;
                        }
                    }
                }
                if step % 3 == 0 {
                    let sizes = [
                        (1, 1),
                        (9, 4),
                        (20, 9),
                        (80, 24),
                        (83, 57),
                        (130, 30),
                        (190, 50),
                    ];
                    let (w, h) = sizes[next(sizes.len() as u32) as usize];
                    screen(&mut app, w, h);
                }
            }
        }
    }
}
