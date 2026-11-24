//! Application state and key handling.

use crate::api::{self, Choice, Detail, Item, Page, Query, Repos, Reviews};
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
    Teams(Result<Vec<String>>),
    Reviews(Result<Vec<Reviews>>),
    List {
        tab: Tab,
        job: u64,
        append: bool,
        result: Result<Page>,
    },
    Doc {
        job: u64,
        bottom: bool,
        result: Result<Detail>,
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
    pub filters: Vec<Choice>,
    pub states: Vec<Choice>,
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
    Org,
    Scope,
    Filter,
    State,
    Target(Act),
    Workflow(String),
}

pub struct Picker {
    pub title: String,
    pub items: Vec<Choice>,
    /// Marked values; only used when several can be chosen.
    pub marked: Vec<String>,
    pub find: String,
    pub sel: usize,
    pub pick: Pick,
    touched: bool,
    moved: bool,
}

impl Picker {
    pub fn multi(&self) -> bool {
        matches!(
            self.pick,
            Pick::Org | Pick::Scope | Pick::Filter | Pick::State
        )
    }

    /// Whether the row's box is checked; "All" means nothing else is.
    pub fn is_marked(&self, value: &str) -> bool {
        if value.is_empty() {
            self.marked.is_empty()
        } else {
            self.marked.iter().any(|m| m == value)
        }
    }

    fn toggle(&mut self) {
        let Some(value) = self.view().get(self.sel).map(|i| i.1.clone()) else {
            return;
        };
        self.touched = true;
        if value.is_empty() {
            self.marked.clear();
        } else if self.is_marked(&value) {
            self.marked.retain(|m| m != &value);
        } else {
            self.marked.push(value);
        }
    }

    /// The choices to apply, or `None` when nothing was changed. Without any
    /// mark toggled, the highlighted row alone is chosen.
    fn chosen(&self) -> Option<Vec<Choice>> {
        let marked: Vec<String> = match (self.touched, self.moved) {
            (true, _) => self.marked.clone(),
            (false, true) => vec![self.view().get(self.sel)?.1.clone()],
            (false, false) => return None,
        };
        Some(
            self.items
                .iter()
                .filter(|i| !i.1.is_empty() && marked.contains(&i.1))
                .cloned()
                .collect(),
        )
    }

    pub fn view(&self) -> Vec<&Choice> {
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
    /// Chosen organizations (or the user).
    pub orgs: Vec<Choice>,
    /// Chosen repositories.
    pub scope: Vec<Choice>,
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
            lists: Tab::ALL.map(|tab| List {
                states: api::default_states(tab),
                ..List::default()
            }),
            orgs: Vec::new(),
            scope: scope.into_iter().map(|s| (s.clone(), s)).collect(),
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
        let repos = match &self.repos {
            Some(r) => r.clone(),
            None if !self.scope.is_empty() => Repos::default(),
            None => {
                // Continues when the repository list arrives.
                self.lists[tab as usize].loading = true;
                return;
            }
        };
        let (orgs, scope) = (self.orgs.clone(), self.scope.clone());
        let list = &mut self.lists[tab as usize];
        list.job += 1;
        list.loading = true;
        let job = list.job;
        let query = Query {
            tab,
            orgs,
            repos: scope,
            filters: list.filters.clone(),
            states: list.states.clone(),
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
        self.doc = Doc::plain(&text);
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
        self.doc = Doc::plain(&text);
        let job = self.doc_job;
        self.spawn(move || Msg::Doc {
            job,
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
                if first && self.scope.is_empty() {
                    for tab in Tab::ALL {
                        if self.lists[tab as usize].loading {
                            self.load(tab, false);
                        }
                    }
                }
                if first {
                    let login = self.repos.as_ref().map(|r| r.login.clone());
                    let login = login.unwrap_or_default();
                    self.spawn(move || Msg::Teams(api::teams(&login)));
                }
                if cursor.is_some() && wanted {
                    self.load_repos(cursor);
                }
            }
            Msg::Teams(Ok(teams)) => {
                if let Some(repos) = &mut self.repos {
                    repos.teams = teams;
                }
            }
            Msg::Teams(Err(e)) => self.error(format!("cannot list your teams: {e}")),
            Msg::Reviews(Ok(reviews)) => self.reviewed(reviews),
            Msg::Reviews(Err(e)) => self.error(format!("cannot load reviews: {e}")),
            Msg::Repos(Err(e)) => {
                if self.repos.is_none() && self.scope.is_empty() {
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
                bottom,
                result,
            } if job == self.doc_job => {
                self.doc_loading = false;
                match result {
                    Ok(d) => {
                        let top = self.doc.top;
                        self.doc = Doc::new(&d.head, &d.body, d.diff.as_deref(), &d.tail);
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
                            0,
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
                let mut items = page.items;
                let mut ids = Vec::new();
                for item in &mut items {
                    if let Item::Ticket(t) = item
                        && !t.id.is_empty()
                    {
                        // Show the previous reviews until the new ones arrive.
                        t.reviews = list.items.iter().find_map(|i| match i {
                            Item::Ticket(old) if old.id == t.id => old.reviews.clone(),
                            _ => None,
                        });
                        ids.push(t.id.clone());
                    }
                }
                if append {
                    list.items.extend(items);
                } else {
                    list.items = items;
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
                if !ids.is_empty() {
                    self.spawn(move || Msg::Reviews(api::reviews(&ids)));
                }
                if let Some(w) = page.warning {
                    self.error(w);
                }
            }
            Err(e) => self.error(format!("{}: {e}", tab.title())),
        }
    }

    fn reviewed(&mut self, reviews: Vec<Reviews>) {
        let list = &mut self.lists[Tab::Prs as usize];
        for review in reviews {
            for item in &mut list.items {
                if let Item::Ticket(t) = item
                    && t.id == review.id
                {
                    t.reviews = Some(review);
                    break;
                }
            }
        }
        let keep = list.selected().map(Item::id);
        list.refilter(); // reviewers are searchable
        if let Some(pos) =
            keep.and_then(|id| list.view.iter().position(|&i| list.items[i].id() == id))
        {
            list.sel = pos;
        }
        if self.tab == Tab::Prs {
            self.sync_doc();
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
                    p.moved = true;
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
            Act::Repo => self.pick_scope(),
            Act::Find => {
                self.detail_focus = false;
                self.finding = true;
            }
            Act::Org => self.pick_org(),
            Act::Filter => {
                let teams = self.repos.as_ref().map_or(&[][..], |r| &r.teams);
                let items = api::filters(self.tab, teams);
                let marked = self.list().filters.clone();
                self.multi_picker("Filter", items, &marked, Pick::Filter);
            }
            Act::NextFile | Act::PrevFile => {
                if self.doc.has_files() {
                    self.detail_focus = true;
                    let height = self.doc_height;
                    self.doc.jump(act == Act::NextFile, height);
                } else if !self.doc_loading {
                    self.open_doc(Act::Diff);
                }
            }
            Act::State => {
                let items = api::states(self.tab);
                let marked = self.list().states.clone();
                self.multi_picker("State", items, &marked, Pick::State);
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
            Act::New | Act::Trigger => match self.scope.as_slice() {
                [(_, repo)] => self.target(act, repo.clone()),
                _ => self.pick_target(act),
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

    /// Choose the repository an action applies to.
    fn pick_target(&mut self, act: Act) {
        let mut items: Vec<Choice> = Vec::new();
        if let Some(item) = self.list().selected() {
            // The selected item's repository is the likely target.
            items.push((item.repo().into(), item.repo().into()));
        }
        let names = if self.scope.is_empty() {
            self.known_repos()
        } else {
            self.scope.clone()
        };
        for name in names {
            if !items.contains(&name) {
                items.push(name);
            }
        }
        if items.is_empty() {
            self.error("the repository list is not loaded yet; press R to retry");
            return;
        }
        self.picker("Choose repository".into(), items, 0, Pick::Target(act));
    }

    /// Known repositories of the chosen organizations.
    fn known_repos(&self) -> Vec<Choice> {
        let names = self.repos.iter().flat_map(|r| &r.names);
        names
            .filter(|n| self.in_orgs(n))
            .map(|n| (n.clone(), n.clone()))
            .collect()
    }

    fn in_orgs(&self, repo: &str) -> bool {
        let owner = repo.split('/').next().unwrap_or("");
        self.orgs.is_empty() || self.orgs.iter().any(|o| o.1 == owner)
    }

    fn pick_scope(&mut self) {
        if self.repos.is_none() {
            self.error("the repository list is not loaded yet; press R to retry");
            return;
        }
        let mut items = self.known_repos();
        // Chosen ones first; one from the command line may not be in the list.
        items.retain(|i| !self.scope.contains(i));
        items.splice(0..0, self.scope.iter().cloned());
        let marked = self.scope.clone();
        self.multi_picker("Repositories", items, &marked, Pick::Scope);
    }

    fn pick_org(&mut self) {
        let Some(repos) = &self.repos else {
            self.error("the organization list is not loaded yet; press R to retry");
            return;
        };
        let orgs = repos.orgs.iter().map(|o| (o.clone(), o.clone()));
        let me = (format!("{} (personal)", repos.login), repos.login.clone());
        let items = orgs.chain([me]).collect();
        let marked = self.orgs.clone();
        self.multi_picker("Organizations", items, &marked, Pick::Org);
    }

    /// A picker for several choices, with an "All" row. Tabs without choices
    /// have none to pick.
    fn multi_picker(&mut self, title: &str, items: Vec<Choice>, marked: &[Choice], pick: Pick) {
        if items.is_empty() {
            return;
        }
        let mut all = vec![("All".to_string(), String::new())];
        all.extend(items);
        let sel = all.iter().position(|i| marked.contains(i));
        self.picker(title.into(), all, sel.unwrap_or_default(), pick);
        if let Some(Modal::Picker(p)) = &mut self.modal {
            p.marked = marked.iter().map(|m| m.1.clone()).collect();
        }
    }

    fn picker(&mut self, title: String, items: Vec<Choice>, sel: usize, pick: Pick) {
        self.modal = Some(Modal::Picker(Picker {
            title,
            items,
            marked: Vec::new(),
            find: String::new(),
            sel,
            pick,
            touched: false,
            moved: false,
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
                    KeyCode::Enter if p.multi() => {
                        if let Some(chosen) = p.chosen() {
                            self.apply(p.pick, chosen);
                        }
                        return;
                    }
                    KeyCode::Char('x') if ctrl && p.multi() => {
                        p.marked.clear();
                        p.touched = true;
                    }
                    KeyCode::Char(' ') if p.multi() => p.toggle(),
                    KeyCode::Enter => {
                        let picked = p.view().get(p.sel).map(|i| i.1.clone());
                        if let Some(value) = picked {
                            return self.picked(p.pick, value);
                        }
                    }
                    KeyCode::Down => {
                        p.sel = (p.sel + 1).min(count.saturating_sub(1));
                        p.moved = true;
                    }
                    KeyCode::Up => {
                        p.sel = p.sel.saturating_sub(1);
                        p.moved = true;
                    }
                    KeyCode::Char(_) | KeyCode::Backspace => {
                        let mut end = p.find.len();
                        form::edit(&mut p.find, &mut end, false, key);
                        p.sel = 0;
                        p.moved = true;
                    }
                    _ => {}
                }
                self.modal = Some(Modal::Picker(p));
            }
        }
    }

    /// Organization or repository changed: every tab loads again when shown.
    fn clear_lists(&mut self) {
        for list in &mut self.lists {
            *list = List {
                filters: std::mem::take(&mut list.filters),
                states: std::mem::take(&mut list.states),
                job: list.job + 1, // ignore lists still in flight
                ..List::default()
            };
        }
        self.reset();
    }

    fn apply(&mut self, pick: Pick, chosen: Vec<Choice>) {
        match pick {
            Pick::Org => {
                self.orgs = chosen;
                let mut scope = std::mem::take(&mut self.scope);
                scope.retain(|r| self.in_orgs(&r.1));
                self.scope = scope;
                self.clear_lists();
            }
            Pick::Scope => {
                self.scope = chosen;
                self.clear_lists();
            }
            Pick::Filter => {
                self.list_mut().filters = chosen;
                self.reset();
            }
            Pick::State => {
                self.list_mut().states = chosen;
                self.reset();
            }
            Pick::Target(_) | Pick::Workflow(_) => {}
        }
    }

    fn picked(&mut self, pick: Pick, value: String) {
        match pick {
            Pick::Org | Pick::Scope | Pick::Filter | Pick::State => {}
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

    #[test]
    fn pickers_take_several_choices() {
        let mut app = app();
        app.scope.clear();
        app.repos = Some(Repos {
            login: "me".into(),
            orgs: vec!["a".into(), "b".into()],
            names: vec!["a/x".into(), "b/y".into(), "me/z".into()],
            ..Repos::default()
        });
        let values =
            |list: &[Choice]| -> Vec<String> { list.iter().map(|c| c.1.clone()).collect() };
        let keys = |app: &mut App, keys: &[KeyCode]| keys.iter().for_each(|k| press(app, *k));
        let (space, down) = (KeyCode::Char(' '), KeyCode::Down);
        // Two marks.
        keys(
            &mut app,
            &[KeyCode::Char('o'), down, space, down, space, KeyCode::Enter],
        );
        assert_eq!(values(&app.orgs), ["a", "b"]);
        // Repositories are limited to the chosen organizations.
        assert_eq!(values(&app.known_repos()), ["a/x", "b/y"]);
        keys(
            &mut app,
            &[KeyCode::Char('p'), KeyCode::Char('y'), KeyCode::Enter],
        );
        assert_eq!(values(&app.scope), ["b/y"]);
        // Enter right away changes nothing, Esc neither.
        keys(&mut app, &[KeyCode::Char('o'), KeyCode::Enter]);
        keys(&mut app, &[KeyCode::Char('o'), down, space, KeyCode::Esc]);
        assert_eq!(values(&app.orgs), ["a", "b"]);
        // Moving without marks chooses just that row; repositories elsewhere go.
        keys(
            &mut app,
            &[KeyCode::Char('o'), KeyCode::Up, down, KeyCode::Enter],
        );
        assert_eq!(values(&app.orgs), ["a"]);
        assert!(app.scope.is_empty());
        // "All" clears.
        keys(
            &mut app,
            &[KeyCode::Char('o'), KeyCode::Up, space, KeyCode::Enter],
        );
        assert!(app.orgs.is_empty());
        keys(&mut app, &[KeyCode::Char('o'), down, space, KeyCode::Enter]);
        assert_eq!(values(&app.orgs), ["a"]);
        press(&mut app, KeyCode::Char('o'));
        app.key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL));
        press(&mut app, KeyCode::Enter);
        assert!(app.orgs.is_empty());
        // States: unmark Open, mark Merged and Closed.
        keys(
            &mut app,
            &[
                KeyCode::Char('s'),
                space,
                down,
                space,
                down,
                space,
                KeyCode::Enter,
            ],
        );
        assert_eq!(app.list().states.len(), 2);
        assert_eq!(app.list().states[0].0, "Merged");
    }
}
