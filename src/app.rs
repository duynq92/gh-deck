//! Application state and key handling.

use crate::api::{self, Item, Page, Query, Repos};
use crate::doc::Doc;
use crate::form::{self, Form};
use crate::gh::{self, Cmd, GhError, Result};
use crate::keys::{self, Act, Tab};
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::thread;

pub enum Msg {
    Input(std::io::Result<Event>),
    Repos(Result<(Repos, Option<String>)>),
    List {
        tab: Tab,
        job: u64,
        append: bool,
        result: Result<Page>,
    },
    Doc {
        job: u64,
        diff: bool,
        bottom: bool,
        result: Result<String>,
    },
    Workflows {
        job: u64,
        repo: String,
        result: Result<Vec<(String, String)>>,
    },
    Done {
        tab: Tab,
        label: String,
        form: Option<Box<Form>>,
        result: Result<String>,
    },
}

#[derive(Default)]
pub struct List {
    pub items: Vec<Item>,
    /// Indices into `items` that match `find`.
    pub view: Vec<usize>,
    pub sel: usize,
    pub top: usize,
    pub total: usize,
    pub cursor: Option<String>,
    pub loading: bool,
    pub loaded: bool,
    pub filter: usize,
    pub state: usize,
    pub find: String,
    job: u64,
}

impl List {
    pub fn selected(&self) -> Option<&Item> {
        self.view.get(self.sel).map(|&i| &self.items[i])
    }

    fn refilter(&mut self) {
        let needle = self.find.to_lowercase();
        self.view = (0..self.items.len())
            .filter(|&i| needle.is_empty() || self.items[i].matches(&needle))
            .collect();
        self.sel = self.sel.min(self.view.len().saturating_sub(1));
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Pick {
    Scope,
    Target(Act),
    Workflow(String),
}

pub struct Picker {
    pub title: String,
    /// (label, value)
    pub items: Vec<(String, String)>,
    pub find: String,
    pub sel: usize,
    pub pick: Pick,
}

impl Picker {
    pub fn view(&self) -> Vec<&(String, String)> {
        let needle = self.find.to_lowercase();
        self.items
            .iter()
            .filter(|i| i.0.to_lowercase().contains(&needle))
            .collect()
    }
}

pub enum Modal {
    Help,
    Confirm(Cmd),
    Form(Box<Form>),
    Picker(Picker),
}

pub enum Status {
    Info(String),
    Error(String),
}

pub struct App {
    tx: Sender<Msg>,
    pub tab: Tab,
    pub lists: [List; 4],
    pub scope: Option<String>,
    pub repos: Option<Repos>,
    pub modal: Option<Modal>,
    pub finding: bool,
    pub detail_focus: bool,
    pub doc: Doc,
    pub doc_act: Act,
    /// Item the document belongs to.
    doc_id: String,
    pub doc_loading: bool,
    doc_job: u64,
    pick_job: u64,
    pub busy: Option<String>,
    pub status: Option<Status>,
    pub quit: bool,
    /// Heights of the last drawn panes, for paging.
    pub list_height: usize,
    pub doc_height: usize,
}

impl App {
    pub fn new(tx: Sender<Msg>, scope: Option<String>) -> Self {
        App {
            tx,
            tab: Tab::Prs,
            lists: Default::default(),
            scope,
            repos: None,
            modal: None,
            finding: false,
            detail_focus: false,
            doc: Doc::default(),
            doc_act: Act::Info,
            doc_id: String::new(),
            doc_loading: false,
            doc_job: 0,
            pick_job: 0,
            busy: None,
            status: None,
            quit: false,
            list_height: 1,
            doc_height: 1,
        }
    }

    pub fn start(&mut self) {
        self.load_repos(None);
        self.load(self.tab, false);
    }

    fn load_repos(&mut self, after: Option<String>) {
        self.spawn(move || Msg::Repos(api::repos(after.as_deref())));
    }

    pub fn list(&self) -> &List {
        &self.lists[self.tab as usize]
    }

    fn list_mut(&mut self) -> &mut List {
        &mut self.lists[self.tab as usize]
    }

    fn spawn(&mut self, work: impl FnOnce() -> Msg + Send + 'static) {
        let tx = self.tx.clone();
        let spawned = thread::Builder::new().stack_size(256 << 10).spawn(move || {
            // Sending only fails when the app is already shutting down.
            if tx.send(work()).is_err() {}
        });
        if let Err(e) = spawned {
            self.error(format!("cannot start worker: {e}"));
        }
    }

    fn error(&mut self, text: impl ToString) {
        self.status = Some(Status::Error(text.to_string()));
    }

    fn info(&mut self, text: impl ToString) {
        self.status = Some(Status::Info(text.to_string()));
    }

    fn load(&mut self, tab: Tab, append: bool) {
        let scope = self.scope.clone();
        let repos = match (&self.repos, &scope) {
            (Some(r), _) => r.clone(),
            (None, Some(_)) => Repos::default(),
            (None, None) => {
                // Continues when the repository list arrives.
                self.lists[tab as usize].loading = true;
                return;
            }
        };
        let list = &mut self.lists[tab as usize];
        list.job += 1;
        list.loading = true;
        let job = list.job;
        let query = Query {
            tab,
            scope,
            filter: list.filter,
            state: list.state,
            cursor: list.cursor.clone().filter(|_| append),
        };
        self.spawn(move || Msg::List {
            tab,
            job,
            append,
            result: api::list(&query, &repos),
        });
    }

    /// Drop the current rows and load again (scope, filter or state changed).
    fn reset(&mut self) {
        let list = self.list_mut();
        list.items.clear();
        list.refilter();
        list.cursor = None;
        self.sync_doc();
        self.load(self.tab, false);
    }

    /// Show the preview of the selected item unless its document is open.
    fn sync_doc(&mut self) {
        let id = self.list().selected().map(Item::id).unwrap_or_default();
        if id == self.doc_id && self.doc_act != Act::Info {
            return;
        }
        if id != self.doc_id {
            self.doc_job += 1; // ignore documents still in flight
            self.doc_loading = false;
            self.detail_focus = false;
        } else if self.detail_focus || self.doc_loading {
            return;
        }
        let text = self
            .list()
            .selected()
            .map(Item::preview)
            .unwrap_or_default();
        self.doc = Doc::new(&text, false);
        self.doc_act = Act::Info;
        self.doc_id = id;
    }

    fn open_doc(&mut self, act: Act) {
        let Some(item) = self.list().selected().cloned() else {
            return;
        };
        self.doc_job += 1;
        self.doc_loading = true;
        self.doc_act = act;
        self.detail_focus = true;
        let text = if act == Act::Info {
            item.preview()
        } else {
            String::new()
        };
        self.doc = Doc::new(&text, false);
        let job = self.doc_job;
        self.spawn(move || Msg::Doc {
            job,
            diff: act == Act::Diff,
            bottom: matches!(act, Act::Log | Act::FailedLog),
            result: api::detail(act, &item),
        });
    }

    fn execute(&mut self, cmd: Cmd, form: Option<Box<Form>>) {
        if let Some(busy) = &self.busy {
            self.status = Some(Status::Error(format!("still running: {busy}")));
            if let Some(form) = form {
                self.modal = Some(Modal::Form(form));
            }
            return;
        }
        self.busy = Some(cmd.label.clone());
        self.status = None;
        let tab = self.tab;
        self.spawn(move || Msg::Done {
            tab,
            result: gh::run(&cmd.args, cmd.stdin.as_deref(), gh::JSON),
            label: cmd.label,
            form,
        });
    }

    pub fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Input(Ok(event)) => self.input(event),
            Msg::Input(Err(e)) => {
                eprintln!("terminal input failed: {e}");
                self.quit = true;
            }
            Msg::Repos(Ok((page, cursor))) => {
                let first = self.repos.is_none();
                let repos = self.repos.get_or_insert_default();
                repos.login = page.login;
                repos.orgs = page.orgs;
                repos.names.extend(page.names);
                let wanted = repos.names.len() < api::MAX_REPOS;
                // Lists need only the first page, so they start right away.
                if first && self.scope.is_none() {
                    for tab in Tab::ALL {
                        if self.lists[tab as usize].loading {
                            self.load(tab, false);
                        }
                    }
                }
                if cursor.is_some() && wanted {
                    self.load_repos(cursor);
                }
            }
            Msg::Repos(Err(e)) => {
                if self.repos.is_none() && self.scope.is_none() {
                    for list in &mut self.lists {
                        list.loading = false;
                    }
                }
                self.error(e);
            }
            Msg::List {
                tab,
                job,
                append,
                result,
            } => self.listed(tab, job, append, result),
            Msg::Doc {
                job,
                diff,
                bottom,
                result,
            } if job == self.doc_job => {
                self.doc_loading = false;
                match result {
                    Ok(text) => {
                        let top = self.doc.top;
                        self.doc = Doc::new(&text, diff);
                        // Logs are read from the end; rendering clamps this.
                        self.doc.top = if bottom { usize::MAX } else { top };
                    }
                    Err(e) => {
                        self.doc_act = Act::Info;
                        self.detail_focus = false;
                        self.sync_doc();
                        self.error(e);
                    }
                }
            }
            Msg::Workflows { job, repo, result } if job == self.pick_job => {
                self.busy = None;
                match result {
                    Ok(items) if items.is_empty() => {
                        self.error(format!("{repo} has no active workflows"));
                    }
                    Ok(items) if self.modal.is_none() => {
                        self.finding = false;
                        self.picker(
                            format!("Trigger workflow in {repo}"),
                            items,
                            Pick::Workflow(repo),
                        );
                    }
                    Ok(_) => {}
                    Err(e) => self.error(e),
                }
            }
            Msg::Doc { .. } | Msg::Workflows { .. } => {}
            Msg::Done {
                tab,
                label,
                form,
                result,
            } => {
                self.busy = None;
                match result {
                    Ok(out) => {
                        let last = out.trim().lines().last().unwrap_or("").to_string();
                        self.info(format!("✓ {label} {last}"));
                        self.load(tab, false);
                    }
                    Err(e) => {
                        self.error(format!("{label}: {e}"));
                        // Give the typed text back, unless something else is open.
                        if let (Some(mut form), None) = (form, &self.modal) {
                            form.error = Some(e.to_string());
                            self.finding = false;
                            self.modal = Some(Modal::Form(form));
                        }
                    }
                }
            }
        }
    }

    fn listed(&mut self, tab: Tab, job: u64, append: bool, result: Result<Page>) {
        let list = &mut self.lists[tab as usize];
        if job != list.job {
            return;
        }
        list.loading = false;
        match result {
            Ok(page) => {
                let keep = list.selected().map(Item::id);
                if append {
                    list.items.extend(page.items);
                } else {
                    list.items = page.items;
                }
                list.loaded = true;
                list.total = page.total;
                list.cursor = page.cursor;
                list.refilter();
                if let Some(pos) =
                    keep.and_then(|id| list.view.iter().position(|&i| list.items[i].id() == id))
                {
                    list.sel = pos;
                }
                if tab == self.tab {
                    self.sync_doc();
                }
                if let Some(w) = page.warning {
                    self.error(w);
                }
            }
            Err(e) => self.error(format!("{}: {e}", tab.title())),
        }
    }

    fn input(&mut self, event: Event) {
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => self.key(key),
            Event::Paste(text) => match &mut self.modal {
                Some(Modal::Form(form)) => form.paste(&text),
                Some(Modal::Picker(p)) => {
                    p.find.extend(text.chars().filter(|c| !c.is_control()));
                    p.sel = 0;
                }
                None if self.finding => {
                    let list = self.list_mut();
                    list.find.extend(text.chars().filter(|c| !c.is_control()));
                    list.refilter();
                    self.sync_doc();
                }
                _ => {}
            },
            _ => {}
        }
    }

    fn key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if let Some(modal) = self.modal.take() {
            self.modal_key(modal, key);
            return;
        }
        if self.finding {
            self.find_key(key);
            return;
        }
        self.status = None;
        match key.code {
            KeyCode::Char('d') if ctrl => self.step(self.page() / 2),
            KeyCode::Char('u') if ctrl => self.step(-self.page() / 2),
            KeyCode::Char(_) if ctrl => {}
            KeyCode::Char('j') | KeyCode::Down => self.step(1),
            KeyCode::Char('k') | KeyCode::Up => self.step(-1),
            KeyCode::PageDown | KeyCode::Char(' ') => self.step(self.page()),
            KeyCode::PageUp => self.step(-self.page()),
            KeyCode::Char('g') | KeyCode::Home => self.step(isize::MIN),
            KeyCode::Char('G') | KeyCode::End => self.step(isize::MAX),
            KeyCode::Tab => self.switch((self.tab as usize + 1) % 4),
            KeyCode::BackTab => self.switch((self.tab as usize + 3) % 4),
            KeyCode::Char(c @ '1'..='4') => self.switch(c as usize - '1' as usize),
            KeyCode::Enter if !self.detail_focus => self.open_doc(Act::Info),
            KeyCode::Esc => self.back(),
            KeyCode::Char(c) => {
                if let Some(act) = keys::lookup(self.tab, c) {
                    self.act(act);
                }
            }
            _ => {}
        }
    }

    fn back(&mut self) {
        if self.detail_focus {
            self.detail_focus = false;
            self.doc_loading = false;
            self.doc_job += 1;
            self.doc_act = Act::Info;
            self.sync_doc();
        } else if !self.list().find.is_empty() {
            self.list_mut().find.clear();
            self.list_mut().refilter();
            self.sync_doc();
        }
    }

    fn page(&self) -> isize {
        let height = if self.detail_focus {
            self.doc_height
        } else {
            self.list_height
        };
        height.max(1) as isize
    }

    fn step(&mut self, by: isize) {
        if self.detail_focus {
            let height = self.doc_height;
            self.doc.scroll(by, height);
            return;
        }
        let list = self.list_mut();
        let max = list.view.len().saturating_sub(1);
        list.sel = list.sel.saturating_add_signed(by).min(max);
        self.sync_doc();
    }

    fn switch(&mut self, to: usize) {
        self.tab = Tab::ALL[to];
        self.detail_focus = false;
        self.doc_act = Act::Info;
        self.sync_doc();
        if !self.list().loaded && !self.list().loading {
            self.load(self.tab, false);
        }
    }

    fn find_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter => self.finding = false,
            KeyCode::Esc => {
                self.finding = false;
                self.list_mut().find.clear();
            }
            KeyCode::Down => return self.step(1),
            KeyCode::Up => return self.step(-1),
            _ => {
                let find = &mut self.list_mut().find;
                let mut end = find.len();
                match key.code {
                    // The find box only edits at its end.
                    KeyCode::Char(_) | KeyCode::Backspace => form::edit(find, &mut end, false, key),
                    _ => {}
                }
            }
        }
        self.list_mut().refilter();
        self.sync_doc();
    }

    fn act(&mut self, act: Act) {
        match act {
            Act::Quit => self.quit = true,
            Act::Help => self.modal = Some(Modal::Help),
            Act::Repo => self.pick_repo("Show repository", Pick::Scope),
            Act::Find => {
                self.detail_focus = false;
                self.finding = true;
            }
            Act::Filter => {
                let count = api::filters(self.tab).len();
                let (tab, scoped) = (self.tab, self.scope.is_some());
                let ticket = matches!(tab, Tab::Prs | Tab::Issues);
                if count > 1 && (ticket || !scoped) {
                    let list = self.list_mut();
                    list.filter = (list.filter + 1) % count;
                    if scoped && api::filters(tab)[list.filter].1 == api::MINE {
                        list.filter = (list.filter + 1) % count;
                    }
                    self.reset();
                }
            }
            Act::State => {
                let count = api::states(self.tab).len();
                if count > 1 {
                    let list = self.list_mut();
                    list.state = (list.state + 1) % count;
                    self.reset();
                }
            }
            Act::More => {
                if self.list().cursor.is_some() && !self.list().loading {
                    self.load(self.tab, true);
                } else if !self.list().loading {
                    self.info("everything is loaded");
                }
            }
            Act::Refresh => {
                if self.repos.is_none() {
                    self.load_repos(None);
                }
                self.load(self.tab, false);
                if self.detail_focus {
                    self.open_doc(self.doc_act);
                }
            }
            Act::Info | Act::Diff | Act::Checks | Act::Log | Act::FailedLog => self.open_doc(act),
            Act::New | Act::Trigger => match self.scope.clone() {
                Some(repo) => self.target(act, repo),
                None => self.pick_repo("Choose repository", Pick::Target(act)),
            },
            _ => {
                if let Some(item) = self.list().selected().cloned() {
                    self.item_act(act, item);
                }
            }
        }
    }

    fn item_act(&mut self, act: Act, item: Item) {
        match act {
            Act::Web => {
                if let Err(e) = open_url(item.url()) {
                    self.error(e);
                }
            }
            Act::Checkout => {
                if let Some(busy) = &self.busy {
                    self.status = Some(Status::Error(format!("still running: {busy}")));
                    return;
                }
                let label = format!("Checkout {}", item.id());
                self.busy = Some(label.clone());
                let tab = self.tab;
                self.spawn(move || Msg::Done {
                    tab,
                    label,
                    form: None,
                    result: api::checkout(&item).map(|_| String::new()),
                });
            }
            Act::Merge if item.state() != "open" => {
                self.error(format!("cannot merge a {} pull request", item.state()));
            }
            _ => {
                if let Some(form) = Form::new(self.tab, act, item.repo(), Some(item.clone()), "") {
                    self.modal = Some(Modal::Form(Box::new(form)));
                } else if let Some(cmd) = api::simple(act, &item) {
                    self.modal = Some(Modal::Confirm(cmd));
                } else {
                    self.error(format!("not possible: {} is {}", item.id(), item.state()));
                }
            }
        }
    }

    /// Start an action that needs a repository instead of an item.
    fn target(&mut self, act: Act, repo: String) {
        if act == Act::Trigger {
            if let Some(busy) = &self.busy {
                self.status = Some(Status::Error(format!("still running: {busy}")));
                return;
            }
            self.busy = Some(format!("Loading workflows of {repo}"));
            self.pick_job += 1;
            let job = self.pick_job;
            self.spawn(move || Msg::Workflows {
                job,
                result: api::workflows(&repo),
                repo,
            });
        } else if let Some(form) = Form::new(self.tab, act, &repo, None, "") {
            self.modal = Some(Modal::Form(Box::new(form)));
        }
    }

    fn pick_repo(&mut self, title: &str, pick: Pick) {
        let Some(repos) = &self.repos else {
            self.error("the repository list is not loaded yet; press R to retry");
            return;
        };
        let mut items: Vec<(String, String)> = Vec::with_capacity(repos.names.len() + 1);
        if pick == Pick::Scope {
            items.push(("All repositories".into(), String::new()));
        } else if let Some(item) = self.list().selected() {
            // The selected item's repository is the likely target.
            items.push((item.repo().into(), item.repo().into()));
        }
        for name in &repos.names {
            if !items.iter().any(|i| &i.1 == name) {
                items.push((name.clone(), name.clone()));
            }
        }
        self.picker(title.into(), items, pick);
    }

    fn picker(&mut self, title: String, items: Vec<(String, String)>, pick: Pick) {
        self.modal = Some(Modal::Picker(Picker {
            title,
            items,
            find: String::new(),
            sel: 0,
            pick,
        }));
    }

    /// The modal was taken out; put it back to keep it open.
    fn modal_key(&mut self, modal: Modal, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match modal {
            Modal::Help => {}
            Modal::Confirm(cmd) => match key.code {
                KeyCode::Char('y') => self.execute(cmd, None),
                KeyCode::Char('n') | KeyCode::Esc => {}
                _ => self.modal = Some(Modal::Confirm(cmd)),
            },
            Modal::Form(mut form) => {
                if key.code == KeyCode::Esc {
                    return;
                }
                if ctrl && key.code == KeyCode::Char('s') {
                    match api::submit(&form) {
                        Ok(cmd) => return self.execute(cmd, Some(form)),
                        Err(e) => form.error = Some(e),
                    }
                } else {
                    form.error = None;
                    form.key(key);
                }
                self.modal = Some(Modal::Form(form));
            }
            Modal::Picker(mut p) => {
                let count = p.view().len();
                match key.code {
                    KeyCode::Esc => return,
                    KeyCode::Enter => {
                        let picked = p.view().get(p.sel).map(|i| i.1.clone());
                        if let Some(value) = picked {
                            return self.picked(p.pick, value);
                        }
                    }
                    KeyCode::Down => p.sel = (p.sel + 1).min(count.saturating_sub(1)),
                    KeyCode::Up => p.sel = p.sel.saturating_sub(1),
                    KeyCode::Char(_) | KeyCode::Backspace => {
                        let mut end = p.find.len();
                        form::edit(&mut p.find, &mut end, false, key);
                        p.sel = 0;
                    }
                    _ => {}
                }
                self.modal = Some(Modal::Picker(p));
            }
        }
    }

    fn picked(&mut self, pick: Pick, value: String) {
        match pick {
            Pick::Scope => {
                self.scope = Some(value).filter(|v| !v.is_empty());
                for list in &mut self.lists {
                    *list = List {
                        filter: list.filter,
                        state: list.state,
                        job: list.job + 1, // ignore lists still in flight
                        ..List::default()
                    };
                }
                self.reset();
            }
            Pick::Target(act) => self.target(act, value),
            Pick::Workflow(repo) => {
                if let Some(form) = Form::new(self.tab, Act::Trigger, &repo, None, &value) {
                    self.modal = Some(Modal::Form(Box::new(form)));
                }
            }
        }
    }
}

fn open_url(url: &str) -> Result<()> {
    if !url.starts_with("https://") {
        return Err(GhError(format!("refusing to open {url:?}")));
    }
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let status = Command::new(opener)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| GhError(format!("{opener}: {e}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(GhError(format!("{opener} failed with {status}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::Ticket;
    use std::sync::mpsc;

    fn app() -> App {
        // Dropping the receiver is fine: no test here waits for a worker.
        App::new(mpsc::channel().0, Some("o/r".into()))
    }

    fn press(app: &mut App, code: KeyCode) {
        app.key(KeyEvent::from(code));
    }

    fn page(numbers: &[u64]) -> Result<Page> {
        Ok(Page {
            total: numbers.len(),
            items: numbers
                .iter()
                .map(|&number| {
                    Item::Ticket(Ticket {
                        repo: "o/r".into(),
                        number,
                        title: format!("title {number}"),
                        state: "OPEN".into(),
                        head: "x".into(),
                        ..Ticket::default()
                    })
                })
                .collect(),
            ..Page::default()
        })
    }

    #[test]
    fn refresh_keeps_selection_and_ignores_stale_lists() {
        let mut app = app();
        app.listed(Tab::Prs, 0, false, page(&[1, 2, 3]));
        press(&mut app, KeyCode::Char('j'));
        app.listed(Tab::Prs, 0, false, page(&[9, 1, 2]));
        assert_eq!(app.list().selected().unwrap().id(), "o/r#2");
        app.listed(Tab::Prs, 77, false, page(&[]));
        assert_eq!(app.list().items.len(), 3);
    }

    #[test]
    fn find_filters_and_escape_clears() {
        let mut app = app();
        app.listed(Tab::Prs, 0, false, page(&[1, 2, 3]));
        for c in "/title 3".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.list().view, [2]);
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.list().view.len(), 3);
    }

    #[test]
    fn destructive_actions_wait_for_y() {
        let mut app = app();
        app.listed(Tab::Prs, 0, false, page(&[1]));
        press(&mut app, KeyCode::Char('X'));
        press(&mut app, KeyCode::Enter);
        assert!(matches!(app.modal, Some(Modal::Confirm(_))));
        press(&mut app, KeyCode::Char('n'));
        assert!(app.modal.is_none() && app.busy.is_none());
    }

    #[test]
    fn failed_submit_returns_the_form() {
        let mut app = app();
        app.listed(Tab::Prs, 0, false, page(&[1]));
        press(&mut app, KeyCode::Char('c'));
        press(&mut app, KeyCode::Char('q')); // typed, not quit
        let Some(Modal::Form(form)) = app.modal.take() else {
            panic!("no form");
        };
        app.handle(Msg::Done {
            tab: Tab::Prs,
            label: "x".into(),
            form: Some(form),
            result: Err(GhError("offline".into())),
        });
        let Some(Modal::Form(form)) = &app.modal else {
            panic!("form lost");
        };
        assert_eq!(form.text(0), "q");
        assert_eq!(form.error.as_deref(), Some("offline"));
        assert!(!app.quit);
    }
}
