//! Drawing. Every label that has a shortcut shows it.

use crate::api::{self, Item};
use crate::app::{App, Modal, Picker, Status};
use crate::doc::clean;
use crate::form::{Form, Kind, line_at};
use crate::keys::{self, Act, GLOBAL, Group, Tab};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const ACCENT: Color = Color::Cyan;
const KEY: Style = Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD);
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
    let actions = action_lines(app, frame.area().width);
    let [tabs, context, body, bar, status] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(actions.len() as u16),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    draw_tabs(frame, app, tabs);
    draw_context(frame, app, context);
    if body.width >= 100 {
        let [left, right] =
            Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                .areas(body);
        draw_list(frame, app, left);
        draw_doc(frame, app, right);
    } else if app.detail_focus {
        draw_doc(frame, app, body);
    } else {
        draw_list(frame, app, body);
    }
    frame.render_widget(Paragraph::new(actions), bar);
    draw_status(frame, app, status);

    match &app.modal {
        Some(Modal::Help) => draw_help(frame, app.tab),
        Some(Modal::Confirm(cmd)) => draw_confirm(frame, &cmd.label),
        Some(Modal::Form(form)) => draw_form(frame, form),
        Some(Modal::Picker(p)) => draw_picker(frame, p),
        None => {}
    }
}

fn draw_tabs(frame: &mut Frame, app: &App, area: Rect) {
    let mut spans = Vec::new();
    for (i, tab) in Tab::ALL.into_iter().enumerate() {
        let style = if tab == app.tab {
            Style::new()
                .fg(ACCENT)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else {
            Style::new()
        };
        spans.extend([
            Span::raw(" "),
            key(&(i + 1).to_string()),
            Span::raw(" "),
            Span::styled(tab.title(), style),
            Span::raw(" "),
        ]);
    }
    frame.render_widget(Line::from(spans), area);
}

fn draw_context(frame: &mut Frame, app: &App, area: Rect) {
    let list = app.list();
    let scope = app.scope.as_deref().unwrap_or("All repositories");
    let mut spans = vec![
        Span::raw(" "),
        key("p"),
        Span::raw(" Repo: "),
        Span::styled(scope, BOLD),
    ];
    let filters = api::filters(app.tab);
    let ticket = matches!(app.tab, Tab::Prs | Tab::Issues);
    // Ownership means nothing once a single repository is chosen.
    if filters.len() > 1 && (ticket || app.scope.is_none()) {
        let name = match (list.filter, &app.scope) {
            (n, Some(_)) if matches!(filters[n].1, "" | api::MINE) => "Everything",
            (n, _) => filters[n].0,
        };
        spans.extend([
            Span::raw("  "),
            key("f"),
            Span::raw(" Filter: "),
            Span::styled(name, BOLD),
        ]);
    }
    let states = api::states(app.tab);
    if states.len() > 1 {
        spans.extend([
            Span::raw("  "),
            key("s"),
            Span::raw(" State: "),
            Span::styled(states[list.state].0, BOLD),
        ]);
    }
    if app.finding || !list.find.is_empty() {
        spans.extend([
            Span::raw("  "),
            key("/"),
            Span::raw(" Find: "),
            Span::styled(list.find.as_str(), BOLD),
        ]);
        if app.finding {
            spans.push(Span::styled("▏", KEY));
            spans.extend(hints(&[("Enter", "Keep"), ("Esc", "Clear")]).spans);
        }
    }
    frame.render_widget(Line::from(spans), area);
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
    block = block.title_bottom(hints(&[
        ("j/k", "Move"),
        ("g/G", "Top/End"),
        ("Enter", "Open"),
    ]));
    let inner = block.inner(area);
    frame.render_widget(block, area);
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
    let width = inner.width as usize;
    let rows: Vec<Line> = list
        .view
        .iter()
        .enumerate()
        .skip(list.top)
        .take(height)
        .map(|(n, &i)| {
            let line = item_line(&list.items[i], width, app.scope.is_some());
            if n == list.sel {
                line.patch_style(Style::new().add_modifier(Modifier::REVERSED))
            } else {
                line
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(rows), inner);
}

fn item_line(item: &Item, width: usize, scoped: bool) -> Line<'static> {
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
    let repo_width = if scoped { 0 } else { (width / 5).min(20) };
    let id_width = if matches!(item, Item::Ticket(_)) {
        6
    } else {
        (width / 6).min(14)
    };
    let date = api::date(item.updated());
    let date = date.get(5..).unwrap_or("");
    let mut spans = vec![
        Span::styled(fit(state, 11), state_style(state)),
        Span::raw(" "),
    ];
    let mut used = 12;
    if repo_width > 0 {
        spans.push(Span::styled(
            fit(repo, repo_width),
            Style::new().fg(Color::Blue),
        ));
        spans.push(Span::raw(" "));
        used += repo_width + 1;
    }
    spans.push(Span::styled(fit(&id, id_width), Style::new().fg(ACCENT)));
    spans.push(Span::raw(" "));
    used += id_width + 1;
    let checks = match item {
        Item::Ticket(t) => check_mark(t.checks.as_deref()),
        _ => None,
    };
    if let Some(mark) = checks {
        spans.push(mark);
        used += 2;
    }
    let tail = if width > used + 32 {
        date.width() + 1
    } else {
        0
    };
    spans.push(Span::raw(fit(&title, width.saturating_sub(used + tail))));
    if tail > 0 {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(date.to_string(), DIM));
    }
    Line::from(spans)
}

fn check_mark(state: Option<&str>) -> Option<Span<'static>> {
    let (mark, color) = match state? {
        "SUCCESS" => ("✓ ", Color::Green),
        "FAILURE" | "ERROR" => ("✗ ", Color::Red),
        _ => ("● ", Color::Yellow),
    };
    Some(Span::styled(mark, Style::new().fg(color)))
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
    if app.doc.len() > height {
        let percent = (app.doc.top + height) * 100 / app.doc.len();
        block = block.title_top(Line::from(format!(" {}% ", percent.min(100))).right_aligned());
    }
    frame.render_widget(block, area);
    let rows: Vec<Line> = app
        .doc
        .visible(height)
        .map(|(text, line)| Line::styled(text, line_style(line, app.doc.diff)))
        .collect();
    frame.render_widget(Paragraph::new(rows), inner);
}

fn line_style(line: &str, diff: bool) -> Style {
    let fg = |c| Style::new().fg(c);
    if !diff {
        return if line.starts_with("── ") {
            fg(ACCENT)
        } else {
            Style::new()
        };
    }
    if line.starts_with("diff ") || line.starts_with("+++") || line.starts_with("---") {
        BOLD
    } else if line.starts_with('+') {
        fg(Color::Green)
    } else if line.starts_with('-') {
        fg(Color::Red)
    } else if line.starts_with("@@") {
        fg(ACCENT)
    } else {
        Style::new()
    }
}

/// The action groups, flowing over as many lines as they need.
fn action_lines(app: &App, width: u16) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let mut spans: Vec<Span> = Vec::new();
    let mut used = 0;
    for group in keys::groups(app.tab).iter().chain([&GLOBAL]) {
        let part = group_spans(app, group);
        let len: usize = part.iter().map(Span::width).sum();
        if used > 0 && used + len > width as usize {
            lines.push(Line::from(std::mem::take(&mut spans)));
            used = 0;
        }
        used += len;
        spans.extend(part);
    }
    lines.push(Line::from(spans));
    lines
}

fn group_spans(app: &App, group: &Group) -> Vec<Span<'static>> {
    let mut spans = vec![Span::styled(
        format!(" {} ", group.name.to_uppercase()),
        Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
    )];
    for k in group.keys {
        let label = match (k.act, app.list().selected().map(Item::state)) {
            (Act::Toggle, Some("open" | "draft")) => "Close",
            (Act::Toggle, Some("closed")) => "Reopen",
            _ => k.label,
        };
        spans.push(key(&k.key.to_string()));
        spans.push(Span::raw(format!("{label} ")));
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
    for group in keys::groups(tab).iter().chain([&GLOBAL]) {
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
    let block = pane(true)
        .title(Line::styled(format!(" Help: {} ", tab.title()), BOLD))
        .title_bottom(hints(&[("any key", "Close")]));
    let inner = popup(frame, 78, lines.len() as u16 + 2, block);
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_picker(frame: &mut Frame, picker: &Picker) {
    let height = frame.area().height.saturating_sub(4).min(24);
    let block = pane(true)
        .title(Line::styled(format!(" {} ", picker.title), BOLD))
        .title_bottom(hints(&[
            ("type", "Find"),
            ("↑/↓", "Move"),
            ("Enter", "Choose"),
            ("Esc", "Cancel"),
        ]));
    let inner = popup(frame, 64, height, block);
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
        lines.push(Line::styled(fit(&item.0, width), style));
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
            for k in keys::groups(tab)
                .iter()
                .chain([&GLOBAL])
                .flat_map(|g| g.keys)
            {
                assert!(
                    text.contains(&format!("[{}]{}", k.key, k.label)),
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
}
