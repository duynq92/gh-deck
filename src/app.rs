//! Application state and key handling.

use crate::ai::{self, Review, Settings, Tool};
use crate::api::Readiness;
use crate::api::{self, Candidates, Choice, Item, Page, PrText, Query, Repos, Reviews};
use crate::config;
use crate::doc::{Doc, Mark, Source};
use crate::form::{self, Form};
use crate::gh::{self, Cmd, GhError, Result};
use crate::keys::{self, Act, Tab};
use crate::review;
use crate::tasks;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
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
        /// Loaded again: stay at the same place.
        refresh: bool,
        result: Result<Source>,
    },
    Workflows {
        job: u64,
        repo: String,
        result: Result<Vec<(String, String)>>,
    },
    Reviewers {
        job: u64,
        repo: String,
        number: u64,
        result: Result<Candidates>,
    },
    Text {
        job: u64,
        act: Act,
        item: Box<Item>,
        result: Result<PrText>,
    },
    Merge {
        job: u64,
        item: Box<Item>,
        result: Result<Readiness>,
    },
    /// Tools, what they do with the code, and why they cannot be used.
    AiTools(Vec<(Tool, String, Option<String>)>),
    Ai {
        job: u64,
        item: String,
        result: Result<Review>,
    },
    Done {
        tab: Tab,
        reload: bool,
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
    /// Loads as soon as the repositories and teams are known.
    waiting: bool,
    pub filters: Vec<Choice>,
    pub states: Vec<Choice>,
    pub find: String,
    /// Events of bots are listed too.
    pub bots: bool,
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
            .filter(|&i| self.bots || !matches!(&self.items[i], Item::Event(e) if e.bot))
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
    Exclude,
    Target(Act),
    Workflow(String),
    Reviewers {
        repo: String,
        number: u64,
        requested: Vec<String>,
    },
    AiTool,
    /// What to do when there are findings already.
    AiAgain,
    /// Checkboxes of a description; values are offsets into `body`.
    Tasks {
        repo: String,
        number: u64,
        body: String,
    },
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
            Pick::Org
                | Pick::Scope
                | Pick::Filter
                | Pick::State
                | Pick::Exclude
                | Pick::Reviewers { .. }
                | Pick::Tasks { .. }
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
            (false, true) => {
                let mut one = vec![self.view().get(self.sel)?.1.clone()];
                // Adding one more must not drop the others.
                if matches!(
                    self.pick,
                    Pick::Exclude | Pick::Reviewers { .. } | Pick::Tasks { .. }
                ) {
                    one.extend(self.marked.iter().cloned());
                }
                one
            }
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
    /// Position in `keys::menu`.
    Menu(usize),
    Message(String),
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
    pub lists: [List; 5],
    /// Chosen organizations (or the user).
    pub orgs: Vec<Choice>,
    /// Chosen repositories.
    pub scope: Vec<Choice>,
    pub repos: Option<Repos>,
    teams_ready: bool,
    /// Teams left out of "my teams".
    pub excluded: Vec<String>,
    /// Where `excluded` is saved; `None` until it was read without trouble.
    config: Option<PathBuf>,
    /// Where the view is saved; `None` until it was read without trouble.
    view_path: Option<PathBuf>,
    saved: config::View,
    pub ai: Settings,
    ai_path: Option<PathBuf>,
    /// What the running AI review is about.
    pub ai_running: Option<String>,
    ai_job: u64,
    /// Action that runs when the changes are loaded.
    pending: Option<Act>,
    /// Comment to show when the document is loaded.
    jump: Option<u64>,
    /// Keys of seen events, oldest first.
    seen: Vec<String>,
    seen_path: Option<PathBuf>,
    /// Events and pull requests with something new.
    pub unread: HashSet<String>,
    pub unread_prs: HashSet<String>,
    /// Findings by item, only until the app ends.
    reviews: HashMap<String, Review>,
    /// A repository was given on the command line: do not save it.
    fixed: bool,
    pub modal: Option<Modal>,
    pub finding: bool,
    pub detail_focus: bool,
    /// The focused document fills the screen.
    pub zoom: bool,
    pub doc: Doc,
    /// What `doc` was made of, to switch between rendered and raw.
    source: Source,
    /// Show Markdown as it was written.
    pub raw: bool,
    pub doc_act: Act,
    /// Item the document belongs to.
    doc_id: String,
    pub doc_loading: bool,
    doc_job: u64,
    pick_job: u64,
    pub busy: Option<String>,
    pub status: Option<Status>,
    /// The latest message, kept after it left the header.
    last_message: Option<String>,
    pub quit: bool,
    /// Heights of the last drawn panes, for paging.
    pub list_height: usize,
    pub doc_height: usize,
}

impl App {
    pub fn new(tx: Sender<Msg>, scope: Option<String>) -> Self {
        let fixed = scope.is_some();
        App {
            tx,
            tab: Tab::Prs,
            lists: Tab::ALL.map(|tab| List {
                filters: api::default_filters(tab),
                states: api::default_states(tab),
                ..List::default()
            }),
            orgs: Vec::new(),
            scope: scope.into_iter().map(|s| (s.clone(), s)).collect(),
            repos: None,
            teams_ready: false,
            excluded: Vec::new(),
            config: None,
            view_path: None,
            saved: config::View::default(),
            ai: Settings::default(),
            ai_path: None,
            ai_running: None,
            ai_job: 0,
            pending: None,
            jump: None,
            seen: Vec::new(),
            seen_path: None,
            unread: HashSet::new(),
            unread_prs: HashSet::new(),
            reviews: HashMap::new(),
            fixed,
            modal: None,
            finding: false,
            detail_focus: false,
            zoom: false,
            doc: Doc::default(),
            source: Source::default(),
            raw: false,
            doc_act: Act::Info,
            doc_id: String::new(),
            doc_loading: false,
            doc_job: 0,
            pick_job: 0,
            busy: None,
            status: None,
            last_message: None,
            quit: false,
            list_height: 1,
            doc_height: 1,
        }
    }

    fn load_config(&mut self) {
        let (Some(teams), Some(view)) = (config::path("excluded-teams"), config::path("view.json"))
        else {
            return self.error("cannot find a config directory: HOME is not set");
        };
        match config::load(&teams) {
            Ok(list) => {
                self.excluded = list;
                self.config = Some(teams);
            }
            Err(e) => self.error(format!("{}: {e}", teams.display())),
        }
        if let Some(path) = config::path("seen.json") {
            match config::load_seen(&path) {
                Ok(seen) => {
                    self.seen = seen;
                    self.seen_path = Some(path);
                }
                Err(e) => self.error(format!("{}: {e}", path.display())),
            }
        }
        if let Some(path) = config::path("ai.json") {
            match config::load_ai(&path) {
                Ok(settings) => {
                    self.ai = settings;
                    self.ai_path = Some(path);
                }
                Err(e) => self.error(format!("{}: {e}", path.display())),
            }
        }
        match config::load_view(&view) {
            Ok(saved) => {
                if let Some(saved) = saved {
                    self.restore(saved);
                }
                self.view_path = Some(view);
            }
            Err(e) => self.error(format!("{}: {e}", view.display())),
        }
    }

    fn restore(&mut self, view: config::View) {
        self.raw = view.raw;
        if !self.fixed {
            self.orgs = view.orgs.clone();
            self.scope = view.repos.clone();
        }
        for (list, (filters, states)) in self.lists.iter_mut().zip(&view.tabs) {
            list.filters = filters.clone();
            list.states = states.clone();
        }
        self.saved = view;
    }

    /// Remember what is shown for the next start.
    fn save_view(&mut self) {
        let Some(path) = &self.view_path else { return };
        if !self.fixed {
            self.saved.orgs = self.orgs.clone();
            self.saved.repos = self.scope.clone();
        }
        let tabs = self.lists.iter();
        self.saved.tabs = tabs
            .map(|l| (l.filters.clone(), l.states.clone()))
            .collect();
        self.saved.raw = self.raw;
        if let Err(e) = config::save_view(path, &self.saved) {
            let text = format!("{}: {e}", path.display());
            self.error(text);
        }
    }

    pub fn start(&mut self) {
        self.load_config();
        self.load_repos(None);
        self.load(self.tab, false);
        self.load(Tab::Activity, false);
    }

    fn load_repos(&mut self, after: Option<String>) {
        self.spawn(move || Msg::Repos(api::repos(after.as_deref())));
    }

    pub fn list(&self) -> &List {
        &self.lists[self.tab as usize]
    }

    /// The selected item; for an event, its pull request.
    pub fn current(&self) -> Option<&Item> {
        self.list().selected().map(|item| match item {
            Item::Event(e) => &*e.pr,
            item => item,
        })
    }

    fn list_mut(&mut self) -> &mut List {
        &mut self.lists[self.tab as usize]
    }

    fn spawn(&mut self, work: impl FnOnce() -> Msg + Send + 'static) {
        if cfg!(test) {
            return; // tests never run `gh`: keys could change real pull requests
        }
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
        let list = &mut self.lists[tab as usize];
        let activity = tab == Tab::Activity;
        let mine = activity || list.filters.iter().any(|f| f.1 == api::MY_TEAMS);
        list.waiting = (mine && !self.teams_ready)
            || (activity && self.repos.is_none())
            || (self.repos.is_none() && self.scope.is_empty());
        if list.waiting {
            list.job += 1; // ignore an older list still in flight
            list.loading = true;
            return;
        }
        let mut repos = self.repos.clone().unwrap_or_default();
        repos.teams.retain(|t| !self.excluded.contains(t));
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

    fn load_waiting(&mut self) {
        for tab in Tab::ALL {
            if self.lists[tab as usize].waiting {
                self.load(tab, false);
            }
        }
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
        let id = self.current().map(Item::id).unwrap_or_default();
        if id == self.doc_id && self.doc_act != Act::Info {
            return;
        }
        if id != self.doc_id {
            self.doc_job += 1; // ignore documents still in flight
            self.pending = None;
            self.doc_loading = false;
            self.detail_focus = false;
            self.zoom = false;
        } else if self.detail_focus || self.doc_loading {
            return;
        }
        let source = self.list().selected().map(Item::source);
        self.show(source.unwrap_or_default());
        self.doc_act = Act::Info;
        self.doc_id = id;
    }

    /// Remember everything on a pull request as seen; "" is all of them.
    fn see(&mut self, id: &str) {
        let events = self.lists[Tab::Activity as usize].items.iter();
        let keys: Vec<String> = events
            .filter_map(|item| match item {
                Item::Event(e) if id.is_empty() || e.pr.id() == id => Some(e.key.clone()),
                _ => None,
            })
            .filter(|key| self.unread.contains(key))
            .collect();
        if keys.is_empty() {
            return;
        }
        self.seen.extend(keys);
        // Far more than one load brings; older ones do not come back.
        let extra = self.seen.len().saturating_sub(3_000);
        self.seen.drain(..extra);
        self.count_unread();
        if let Some(path) = &self.seen_path
            && let Err(e) = config::save_seen(path, &self.seen)
        {
            let text = format!("{}: {e}", path.display());
            self.error(text);
        }
    }

    fn count_unread(&mut self) {
        let list = &self.lists[Tab::Activity as usize];
        let seen: HashSet<&String> = self.seen.iter().collect();
        let mut unread = HashSet::new();
        let mut prs = HashSet::new();
        for item in &list.items {
            if let Item::Event(e) = item
                && (list.bots || !e.bot)
                && !seen.contains(&e.key)
            {
                unread.insert(e.key.clone());
                prs.insert(e.pr.id());
            }
        }
        (self.unread, self.unread_prs) = (unread, prs);
    }

    /// Attach the findings of this item.
    fn show(&mut self, mut source: Source) {
        source.ai = match self.reviews.get(&self.doc_id) {
            // Previews have no changes to put findings on.
            Some(review) if source.diff.is_some() => {
                let mut review = review.clone();
                review.outdated = review.oid != source.oid;
                Some(review)
            }
            _ => None,
        };
        self.doc = Doc::new(&source, self.raw);
        self.source = source;
    }

    /// Switch between rendered and raw, staying at the same file.
    fn toggle_raw(&mut self) {
        self.raw = !self.raw;
        self.save_view();
        let file = self.doc.current_file().map(|f| f.0);
        let width = self.doc.width();
        self.doc = Doc::new(&self.source, self.raw);
        if let Some(n) = file {
            self.doc.layout(width);
            let height = self.doc_height;
            self.doc.show_file(n, height);
        }
    }

    fn open_doc(&mut self, act: Act) {
        let Some(item) = self.current().cloned() else {
            return;
        };
        self.doc_job += 1;
        self.doc_loading = true;
        self.doc_act = act;
        self.detail_focus = true;
        self.jump = match (self.list().selected(), act) {
            (Some(Item::Event(e)), Act::Info | Act::Diff) => e.comment,
            _ => None,
        };
        self.doc_id = item.id();
        self.see(&item.id());
        self.show(if act == Act::Info {
            item.source()
        } else {
            Source::default()
        });
        let job = self.doc_job;
        self.spawn(move || Msg::Doc {
            job,
            bottom: matches!(act, Act::Log | Act::FailedLog),
            refresh: false,
            result: api::detail(act, &item),
        });
    }

    /// Load the open document again without losing the place.
    fn refresh_doc(&mut self) {
        let Some(item) = self.current().cloned() else {
            return;
        };
        self.doc_job += 1;
        self.doc_loading = true;
        let (job, act) = (self.doc_job, self.doc_act);
        self.spawn(move || Msg::Doc {
            job,
            bottom: false,
            refresh: true,
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
            reload: cmd.reload,
            result: gh::execute(&cmd),
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
                if first {
                    self.load_waiting();
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
                self.teams_ready = true;
                self.load_waiting();
            }
            Msg::Teams(Err(e)) => {
                self.teams_ready = true;
                self.load_waiting();
                self.error(format!("cannot list your teams: {e}"));
            }
            Msg::Reviews(Ok(reviews)) => self.reviewed(reviews),
            Msg::Reviews(Err(e)) => self.error(format!("cannot load reviews: {e}")),
            Msg::Repos(Err(e)) => {
                if self.repos.is_none() {
                    for list in self.lists.iter_mut().filter(|l| l.waiting) {
                        list.waiting = false;
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
                refresh,
                result,
            } if job == self.doc_job => {
                self.doc_loading = false;
                match result {
                    Ok(mut d) => {
                        let top = self.doc.top;
                        d.show_resolved = self.source.show_resolved;
                        // Line mode goes on: its keys mean something else outside.
                        let mark = self.doc.mark().map(|mark| match mark {
                            Mark::Finding(n) => Mark::Finding(n),
                            Mark::Thread(n) => {
                                let id = self.source.threads.get(n).map(|t| &t.id);
                                let now = d.threads.iter().position(|t| Some(&t.id) == id);
                                Mark::Thread(now.unwrap_or(usize::MAX))
                            }
                            line => line,
                        });
                        let (width, height) = (self.doc.width(), self.doc_height);
                        self.show(d);
                        // Logs are read from the end; rendering clamps this.
                        self.doc.top = if bottom { usize::MAX } else { top };
                        if let Some(mark) = mark {
                            self.doc.layout(width);
                            self.doc.seek(&mark, height);
                        }
                        if let Some(id) = self.jump.take() {
                            let has =
                                |t: &review::Thread| t.comments.iter().any(|c| c.id == Some(id));
                            if let Some(n) = self.source.threads.iter().position(has) {
                                self.doc.layout(width);
                                self.doc.seek(&Mark::Thread(n), height);
                            }
                        }
                        if let Some(act) = self.pending.take() {
                            // Not again: it would load forever.
                            if self.source.diff.is_some() {
                                self.status = None;
                                self.doc.layout(width);
                                self.act(act);
                            } else {
                                self.error("the changes are not available");
                            }
                        }
                    }
                    Err(e) if refresh => self.error(e),
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
            Msg::Reviewers {
                job,
                repo,
                number,
                result,
            } if job == self.pick_job => {
                self.busy = None;
                match result {
                    Ok(c) if !c.mine => self.error("only your own pull requests"),
                    Ok(c) if self.modal.is_none() => {
                        self.finding = false;
                        self.pick_reviewers(repo, number, c);
                    }
                    Ok(_) => {}
                    Err(e) => self.error(e),
                }
            }
            Msg::Text {
                job,
                act,
                item,
                result,
            } if job == self.pick_job => {
                self.busy = None;
                match result {
                    Ok(text) if !text.mine => self.error("only your own pull requests"),
                    Ok(text) if self.modal.is_none() => {
                        self.finding = false;
                        self.edit_text(act, *item, text);
                    }
                    Ok(_) => {}
                    Err(e) => self.error(e),
                }
            }
            Msg::Merge { job, item, result } if job == self.pick_job => {
                self.busy = None;
                match result {
                    Ok(ready) if self.modal.is_none() => {
                        self.finding = false;
                        self.merge_form(*item, ready);
                    }
                    Ok(_) => {}
                    Err(e) => self.error(e),
                }
            }
            Msg::AiTools(tools) => {
                if self.modal.is_none() {
                    self.finding = false;
                    let items = tools
                        .into_iter()
                        .map(|(tool, what, problem)| match problem {
                            None => (format!("{}: {what}", tool.name()), tool.name().to_string()),
                            Some(why) => (format!("{} ({why})", tool.name()), String::new()),
                        });
                    let items = items.collect();
                    self.picker("Choose the AI tool".into(), items, 0, Pick::AiTool);
                }
            }
            Msg::Ai { job, item, result } if job == self.ai_job => {
                self.ai_running = None;
                match result {
                    Ok(review) => self.ai_done(item, review),
                    Err(e) => self.error(e),
                }
            }
            Msg::Ai { .. }
            | Msg::Merge { .. }
            | Msg::Doc { .. }
            | Msg::Workflows { .. }
            | Msg::Reviewers { .. }
            | Msg::Text { .. } => {}
            Msg::Done {
                tab,
                reload,
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
                        if tab == Tab::Prs {
                            self.load(Tab::Activity, false);
                        }
                        // Posted findings are discussions now.
                        if let Some(form) = form.filter(|f| f.act == Act::AiPost)
                            && let Some(item) = &form.item
                            && let Some(review) = self.reviews.get_mut(&item.id())
                        {
                            review.findings.retain(|f| !f.accepted);
                        }
                        if reload && self.detail_focus && tab == self.tab {
                            self.refresh_doc();
                        }
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
                if tab == Tab::Activity {
                    self.count_unread();
                }
                // GitHub answers two small requests faster than a big one.
                for ids in ids.chunks(25) {
                    let ids = ids.to_vec();
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
        if let Some(Status::Info(text) | Status::Error(text)) = self.status.take() {
            self.last_message = Some(text);
        }
        if self.detail_focus && self.doc.cursor.is_some() && !ctrl {
            return self.line_key(key);
        }
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
            KeyCode::Tab => self.switch((self.tab as usize + 1) % Tab::ALL.len()),
            KeyCode::BackTab => {
                self.switch((self.tab as usize + Tab::ALL.len() - 1) % Tab::ALL.len())
            }
            KeyCode::Char(c @ '1'..='5') => self.switch(c as usize - '1' as usize),
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
            self.zoom = false;
            self.pending = None;
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
        self.zoom = false;
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
            Act::Seen => {
                self.see("");
                self.info("everything is marked as seen");
            }
            Act::Bots => {
                let list = &mut self.lists[Tab::Activity as usize];
                list.bots = !list.bots;
                list.refilter();
                self.count_unread();
                self.sync_doc();
            }
            Act::Raw => self.toggle_raw(),
            Act::Ai => self.ai_key(),
            Act::AiPost => self.ai_post(),
            Act::Lines => self.start_lines(),
            Act::NextThread => self.jump_thread(true),
            Act::PrevThread => self.jump_thread(false),
            Act::Resolved => self.toggle_resolved(),
            Act::Menu => self.modal = Some(Modal::Menu(0)),
            Act::Zoom => {
                self.zoom = !self.zoom;
                if self.zoom && !self.detail_focus {
                    self.open_doc(Act::Info);
                }
                self.zoom &= self.detail_focus; // nothing is selected
            }
            Act::Message => {
                let text = self.last_message.clone();
                self.modal = Some(Modal::Message(text.unwrap_or("no message yet".into())));
            }
            Act::Repo => self.pick_scope(),
            Act::Find => {
                self.detail_focus = false;
                self.zoom = false;
                self.finding = true;
            }
            Act::Org => self.pick_org(),
            Act::Filter => {
                let teams = self.repos.as_ref().map_or(&[][..], |r| &r.teams);
                let items = api::filters(self.tab, teams);
                let marked = self.list().filters.clone();
                self.multi_picker("Filter", "All", items, &marked, Pick::Filter);
            }
            Act::Exclude => self.pick_excluded(),
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
                self.multi_picker("State", "All", items, &marked, Pick::State);
            }
            Act::More => {
                if self.list().cursor.is_some() && !self.list().loading {
                    self.load(self.tab, true);
                } else if !self.list().loading {
                    let (shown, total) = (self.list().items.len(), self.list().total);
                    if matches!(self.tab, Tab::Releases | Tab::Runs) && shown < total {
                        self.info(format!(
                            "showing the newest {shown}; choose fewer repositories with [p] for older ones"
                        ));
                    } else {
                        self.info("everything is loaded");
                    }
                }
            }
            Act::Refresh => {
                if self.repos.is_none() {
                    self.load_repos(None);
                }
                self.load(self.tab, false);
                if self.tab == Tab::Prs {
                    self.load(Tab::Activity, false);
                }
                if self.detail_focus {
                    self.refresh_doc();
                }
            }
            Act::Info | Act::Diff | Act::Checks | Act::Log | Act::FailedLog => self.open_doc(act),
            Act::New | Act::Trigger => match self.scope.as_slice() {
                [(_, repo)] => self.target(act, repo.clone()),
                _ => self.pick_target(act),
            },
            _ => {
                if let Some(item) = self.current().cloned() {
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
                    reload: false,
                    label,
                    form: None,
                    result: api::checkout(&item).map(|_| String::new()),
                });
            }
            Act::Reviewers => {
                let Item::Ticket(t) = &item else { return };
                if !self.is_mine(&item) {
                    return self.error("you can choose reviewers only for PRs opened by you");
                }
                if t.state != "OPEN" {
                    return self.error(format!("{} is {}", item.id(), item.state()));
                }
                if let Some(busy) = &self.busy {
                    self.status = Some(Status::Error(format!("still running: {busy}")));
                    return;
                }
                self.busy = Some(format!("Loading reviewers for {}", item.id()));
                self.pick_job += 1;
                let (job, repo, number) = (self.pick_job, t.repo.clone(), t.number);
                self.spawn(move || Msg::Reviewers {
                    job,
                    result: api::candidates(&repo, number),
                    repo,
                    number,
                });
            }
            Act::Title | Act::Description | Act::Tasks => {
                let Item::Ticket(t) = &item else { return };
                if !self.is_mine(&item) {
                    return self.error("you can edit only PRs opened by you");
                }
                if let Some(busy) = &self.busy {
                    self.status = Some(Status::Error(format!("still running: {busy}")));
                    return;
                }
                self.busy = Some(format!("Loading {}", item.id()));
                self.pick_job += 1;
                let (job, repo, number) = (self.pick_job, t.repo.clone(), t.number);
                self.spawn(move || Msg::Text {
                    job,
                    act,
                    item: Box::new(item),
                    result: api::pr_text(&repo, number),
                });
            }
            Act::Ready if !self.is_mine(&item) => {
                self.error("you can mark only PRs opened by you as ready");
            }
            Act::Ready if item.state() != "draft" => {
                self.error(format!("{} is {}, not a draft", item.id(), item.state()));
            }
            Act::Merge if item.state() != "open" => {
                self.error(format!("cannot merge a {} pull request", item.state()));
            }
            Act::Merge => {
                let Item::Ticket(t) = &item else { return };
                if let Some(busy) = &self.busy {
                    self.status = Some(Status::Error(format!("still running: {busy}")));
                    return;
                }
                self.busy = Some(format!("Checking {}", item.id()));
                self.pick_job += 1;
                let (job, repo, number) = (self.pick_job, t.repo.clone(), t.number);
                self.spawn(move || Msg::Merge {
                    job,
                    item: Box::new(item),
                    result: api::readiness(&repo, number),
                });
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

    /// Whether the user opened the item. Unknown until the login is loaded, and
    /// checked again by GitHub before anything is changed.
    pub fn is_mine(&self, item: &Item) -> bool {
        match (item, &self.repos) {
            (Item::Ticket(t), Some(r)) => t.author.as_ref() == Some(&r.login),
            (Item::Ticket(_), None) => true,
            _ => false,
        }
    }

    fn merge_form(&mut self, item: Item, ready: Readiness) {
        let Item::Ticket(t) = &item else { return };
        let id = item.id();
        if t.oid != ready.oid {
            return self.error(format!(
                "{id} has new commits; press R and review them first"
            ));
        }
        if ready.conflicts {
            return self.error(format!("{id} has conflicts with its base branch"));
        }
        if ready.methods.is_empty() {
            return self.error(format!("{} allows no merge method", t.repo));
        }
        let repo = t.repo.clone();
        if let Some(mut form) = Form::new(Tab::Prs, Act::Merge, &repo, Some(item), "") {
            if let Some(method) = form.fields.first_mut() {
                method.kind = form::Kind::Choice(ready.methods, 0);
            }
            form.notes = ready.notes;
            self.modal = Some(Modal::Form(Box::new(form)));
        }
    }

    /// Run an action once the changes are there, loading them if needed.
    fn when_loaded(&mut self, act: Act, open: Act) {
        if !self.doc_loading {
            self.open_doc(open);
        }
        if self.doc_loading {
            self.pending = Some(act);
            self.info("loading the changes…");
        }
    }

    /// Start line mode, or say why not.
    fn start_lines(&mut self) {
        if !self.doc.has_files() {
            return self.when_loaded(Act::Lines, Act::Diff);
        }
        self.detail_focus = true;
        let height = self.doc_height;
        if !self.doc.start_cursor(height) {
            self.error("there is no line to comment on");
        }
    }

    fn jump_thread(&mut self, forward: bool) {
        if !self.doc.has_files() && !self.doc.has_threads() {
            let act = if forward {
                Act::NextThread
            } else {
                Act::PrevThread
            };
            return self.when_loaded(act, Act::Diff);
        }
        self.detail_focus = true;
        let height = self.doc_height;
        if !self.doc.jump_thread(forward, height) {
            self.info("no discussions on the code");
        }
    }

    fn toggle_resolved(&mut self) {
        self.source.show_resolved = !self.source.show_resolved;
        self.rebuild();
    }

    /// Draw the document again, at the same place.
    fn rebuild(&mut self) {
        let (mark, top) = (self.doc.mark(), self.doc.top);
        let (width, height) = (self.doc.width(), self.doc_height);
        self.doc = Doc::new(&self.source, self.raw);
        self.doc.top = top;
        if let Some(mark) = mark {
            self.doc.layout(width);
            self.doc.seek(&mark, height);
        }
    }

    /// Keys of line mode. Everything else waits until it is left.
    fn line_key(&mut self, key: KeyEvent) {
        let height = self.doc_height;
        let page = height.max(2) as isize / 2;
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.doc.move_cursor(1, height),
            KeyCode::Char('k') | KeyCode::Up => self.doc.move_cursor(-1, height),
            KeyCode::PageDown => self.doc.move_cursor(page, height),
            KeyCode::PageUp => self.doc.move_cursor(-page, height),
            KeyCode::Char('g') | KeyCode::Home => self.doc.move_cursor(isize::MIN + 1, height),
            KeyCode::Char('G') | KeyCode::End => self.doc.move_cursor(isize::MAX, height),
            KeyCode::Char(c @ ('n' | 'N')) => {
                self.doc.jump(c == 'n', height);
                self.doc.start_cursor(height);
            }
            KeyCode::Char('u') => self.jump_thread(true),
            KeyCode::Char('U') => self.jump_thread(false),
            KeyCode::Char('O') => self.toggle_resolved(),
            KeyCode::Char('z') => self.zoom = !self.zoom,
            KeyCode::Char('?') => self.modal = Some(Modal::Help),
            KeyCode::Char('!') => self.act(Act::Message),
            KeyCode::Char('A') => self.ai_key(),
            KeyCode::Char('P') => self.ai_post(),
            KeyCode::Char(c @ (' ' | 'e' | 'd')) if self.doc.finding_at_cursor().is_some() => {
                if let Some(n) = self.doc.finding_at_cursor() {
                    match c {
                        'e' => self.ai_edit(n),
                        _ => self.ai_decide(n, c == 'd'),
                    }
                }
            }
            KeyCode::Enter if self.doc.finding_at_cursor().is_some() => {
                if let Some(n) = self.doc.finding_at_cursor() {
                    self.ai_edit(n);
                }
            }
            KeyCode::Char(' ') | KeyCode::Char('v') => {
                self.doc.anchor = match self.doc.anchor {
                    Some(_) => None,
                    None => self.doc.cursor,
                };
                if let Err(e) = self.doc.place() {
                    self.doc.anchor = None;
                    self.error(e);
                }
            }
            KeyCode::Char('x') => self.toggle_thread(),
            KeyCode::Enter | KeyCode::Char('c') => self.write_here(),
            KeyCode::Esc | KeyCode::Char('l') => {
                self.doc.cursor = None;
                self.doc.anchor = None;
            }
            _ => self.info("line mode: Esc leaves it"),
        }
    }

    fn toggle_thread(&mut self) {
        let at = self.doc.thread_at_cursor();
        let Some(thread) = at.and_then(|n| self.source.threads.get(n)) else {
            return self.info("move to a discussion first: u");
        };
        if thread.can_toggle {
            self.modal = Some(Modal::Confirm(review::toggle(thread)));
        } else {
            self.error("GitHub does not let you resolve this discussion");
        }
    }

    fn ai_key(&mut self) {
        if self.ai_running.take().is_some() {
            self.ai_job += 1; // ignore its answer
            if !cfg!(test) {
                ai::stop(); // would stop the programs of other tests
            }
            return self.info("AI review stopped");
        }
        let Some(Item::Ticket(t)) = self.current() else {
            return;
        };
        let repo = t.repo.clone();
        if self.source.diff.is_none() || self.source.oid.is_empty() {
            return self.when_loaded(Act::Ai, Act::Info);
        }
        if self.ai.blocks(&repo) {
            return self.error(format!("{repo} is blocked in ai.json"));
        }
        let known = self
            .source
            .ai
            .as_ref()
            .is_some_and(|r| !r.findings.is_empty());
        let height = self.doc_height;
        if known && self.doc.finding_at_cursor().is_none() && self.doc.go_to_finding(None, height) {
            self.detail_focus = true;
            return;
        }
        if known {
            let items = vec![
                ("Run again".to_string(), "run".to_string()),
                ("Choose another tool".to_string(), "tool".to_string()),
            ];
            return self.picker("AI review".into(), items, 0, Pick::AiAgain);
        }
        self.ai_start();
    }

    fn ai_start(&mut self) {
        match self.ai.tool {
            Some(tool) => self.ai_run(tool),
            None => self.ai_choose(),
        }
    }

    fn ai_choose(&mut self) {
        let settings = self.ai.clone();
        self.info("looking for AI tools");
        self.spawn(move || Msg::AiTools(ai::tools(&settings)));
    }

    fn ai_run(&mut self, tool: Tool) {
        let Some(Item::Ticket(t)) = self.current().cloned() else {
            return;
        };
        let Some(diff) = &self.source.diff else {
            return self.error("the changes are not loaded");
        };
        // Choosing a tool takes a while; the selection may have moved on.
        let item = Item::Ticket(t.clone()).id();
        if self.ai.blocks(&t.repo) || item != self.doc_id {
            return self.error(format!("not started: {item} is blocked or not open"));
        }
        let files = crate::doc::changed(diff);
        let (prompt, left_out) = match ai::prompt(&t.title, &t.body, &files) {
            Ok(prompt) => prompt,
            Err(e) => return self.error(e),
        };
        self.ai_job += 1;
        self.ai_running = Some(format!(
            "AI review of {item}: {}, {} KB, [A] stops",
            tool.name(),
            prompt.len().div_ceil(1024)
        ));
        let (job, settings, oid) = (self.ai_job, self.ai.clone(), self.source.oid.clone());
        self.spawn(move || Msg::Ai {
            job,
            item,
            result: ai::review(tool, &settings, &prompt)
                .and_then(|answer| ai::parse(&answer, &files))
                .map(|findings| Review {
                    oid,
                    tool: tool.name().into(),
                    findings,
                    left_out,
                    outdated: false,
                }),
        });
    }

    fn ai_done(&mut self, item: String, review: Review) {
        let count = review.findings.len();
        let left = review.left_out.len();
        let here = item == self.doc_id && self.source.diff.is_some();
        let mut text = match count {
            0 => format!("AI review of {item}: no findings"),
            n => format!("AI review of {item}: {n} findings, [A] shows them"),
        };
        if left > 0 {
            text += &format!(" ({left} files not reviewed)");
        }
        self.info(text);
        self.reviews.insert(item, review);
        if here {
            self.ai_refresh();
        }
    }

    /// Show the current findings of the open document.
    fn ai_refresh(&mut self) {
        let outdated = |r: &Review| r.oid != self.source.oid;
        let review = self.reviews.get(&self.doc_id).map(|r| Review {
            outdated: outdated(r),
            ..r.clone()
        });
        self.source.ai = review;
        self.rebuild();
    }

    /// Accept, un-accept or discard the finding under the cursor.
    fn ai_decide(&mut self, n: usize, discard: bool) {
        let Some(review) = self.reviews.get_mut(&self.doc_id) else {
            return;
        };
        let Some(finding) = review.findings.get_mut(n) else {
            return;
        };
        if discard {
            review.findings.remove(n);
            let next = self.doc.finding_next_to(n);
            self.doc.cursor = None;
            self.ai_refresh();
            // Those after the removed one moved up.
            let next = next.map(|next| if next > n { next - 1 } else { next });
            let height = self.doc_height;
            if next.is_some() {
                self.doc.go_to_finding(next, height);
            }
        } else {
            finding.accepted = !finding.accepted;
            self.ai_refresh();
        }
    }

    fn ai_edit(&mut self, n: usize) {
        let Some(item) = self.current().cloned() else {
            return;
        };
        let finding = self
            .reviews
            .get(&self.doc_id)
            .and_then(|r| r.findings.get(n));
        let Some(finding) = finding.cloned() else {
            return;
        };
        let repo = item.repo().to_string();
        if let Some(mut form) = Form::new(Tab::Prs, Act::AiEdit, &repo, Some(item), &n.to_string())
        {
            form.title = format!("Finding on {}", finding.path);
            for (field, text, multi) in [(0, &finding.title, false), (1, &finding.body, true)] {
                if let Some(f) = form.fields.get_mut(field) {
                    f.kind = form::Kind::text(text, multi);
                }
            }
            form.focus = 1;
            self.modal = Some(Modal::Form(Box::new(form)));
        }
    }

    fn ai_edited(&mut self, form: &Form) -> std::result::Result<(), String> {
        let (title, body) = (form.text(0).trim(), form.text(1).trim());
        if title.is_empty() {
            return Err("Title is required".into());
        }
        let review = self.reviews.get_mut(&self.doc_id);
        let n: Option<usize> = form.extra.parse().ok();
        let finding = review.zip(n).and_then(|(r, n)| r.findings.get_mut(n));
        let finding = finding.ok_or("the finding is gone")?;
        finding.title = title.to_string();
        finding.body = body.to_string();
        finding.accepted = true;
        self.ai_refresh();
        Ok(())
    }

    fn ai_post(&mut self) {
        let Some(item) = self.current().cloned() else {
            return;
        };
        let Some(review) = self.source.ai.as_ref().filter(|r| r.accepted() > 0) else {
            return self.error("accept findings first: A, then Space");
        };
        if review.outdated {
            return self.error("the pull request has newer commits; press A to run again");
        }
        let batch = review::Batch::new(review);
        let extra = match serde_json::to_string(&batch) {
            Ok(extra) => extra,
            Err(e) => return self.error(e),
        };
        let repo = item.repo().to_string();
        if let Some(mut form) = Form::new(Tab::Prs, Act::AiPost, &repo, Some(item), &extra) {
            form.notes = batch.notes();
            self.modal = Some(Modal::Form(Box::new(form)));
        }
    }

    /// Reply to the discussion under the cursor, or comment on the line.
    fn write_here(&mut self) {
        let Some(item) = self.current().cloned() else {
            return;
        };
        let (act, extra, what) = if let Some(thread) = self
            .doc
            .thread_at_cursor()
            .and_then(|n| self.source.threads.get(n))
        {
            let first = thread.comments.first().and_then(|c| c.id);
            match first {
                Some(id) if thread.can_reply => (
                    Act::Reply,
                    id.to_string(),
                    format!("Reply on {}", thread.path),
                ),
                _ => return self.error("GitHub does not let you reply here"),
            }
        } else {
            let place = match self.doc.place() {
                Ok(place) => place,
                Err(e) => return self.error(e),
            };
            if self.source.oid.is_empty() {
                return self.error("the commit of these changes is unknown; press Esc, then R");
            }
            let what = format!("Comment on {}", place.label());
            let new = review::NewComment {
                commit_id: self.source.oid.clone(),
                place,
            };
            match serde_json::to_string(&new) {
                Ok(extra) => (Act::LineComment, extra, what),
                Err(e) => return self.error(e),
            }
        };
        let repo = item.repo().to_string();
        if let Some(mut form) = Form::new(Tab::Prs, act, &repo, Some(item), &extra) {
            form.title = what;
            self.modal = Some(Modal::Form(Box::new(form)));
        }
    }

    /// Edit what is on GitHub now, not the copy in the list.
    fn edit_text(&mut self, act: Act, mut item: Item, text: PrText) {
        let Item::Ticket(t) = &mut item else { return };
        if act == Act::Tasks {
            return self.pick_tasks(t.repo.clone(), t.number, text.body);
        }
        t.title = text.title;
        t.body = text.body;
        let repo = t.repo.clone();
        if let Some(form) = Form::new(Tab::Prs, act, &repo, Some(item), "") {
            self.modal = Some(Modal::Form(Box::new(form)));
        }
    }

    fn pick_tasks(&mut self, repo: String, number: u64, body: String) {
        let tasks = tasks::find(&body);
        if tasks.is_empty() {
            let text = format!("{repo}#{number} has no checkboxes; [E] edits the description");
            return self.error(text);
        }
        let marked = tasks.iter().filter(|t| t.done);
        let marked = marked.map(|t| t.mark.to_string()).collect();
        let items = tasks
            .into_iter()
            .map(|t| (t.text, t.mark.to_string()))
            .collect();
        let title = format!("Checkboxes of {repo}#{number}");
        let pick = Pick::Tasks { repo, number, body };
        self.picker(title, items, 0, pick);
        if let Some(Modal::Picker(p)) = &mut self.modal {
            p.marked = marked;
        }
    }

    fn pick_reviewers(&mut self, repo: String, number: u64, c: Candidates) {
        let (mut items, others): (Vec<Choice>, Vec<Choice>) = c
            .people
            .into_iter()
            .partition(|p| c.requested.contains(&p.1));
        for login in &c.requested {
            if !items.iter().any(|i| &i.1 == login) {
                items.push((login.clone(), login.clone()));
            }
        }
        items.extend(others);
        if items.is_empty() {
            return self.error(format!("nobody else can review in {repo}"));
        }
        let title = format!("Reviewers of {repo}#{number}");
        let pick = Pick::Reviewers {
            repo,
            number,
            requested: c.requested.clone(),
        };
        self.picker(title, items, 0, pick);
        if let Some(Modal::Picker(p)) = &mut self.modal {
            p.marked = c.requested;
        }
    }

    /// Choose the repository an action applies to.
    fn pick_target(&mut self, act: Act) {
        let mut items: Vec<Choice> = Vec::new();
        if let Some(item) = self.current() {
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
        self.multi_picker("Repositories", "All", items, &marked, Pick::Scope);
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
        self.multi_picker("Organizations", "All", items, &marked, Pick::Org);
    }

    fn pick_excluded(&mut self) {
        if !self.teams_ready {
            return self.error("your teams are not loaded yet; press R to retry");
        }
        if self.config.is_none() {
            return self.error("the saved list could not be read; fix it and start again");
        }
        let teams = self.repos.iter().flat_map(|r| &r.teams);
        // Excluded first, also those the user has left meanwhile.
        let mut names = self.excluded.clone();
        names.extend(teams.filter(|t| !self.excluded.contains(t)).cloned());
        if names.is_empty() {
            return self.error("you are in no team");
        }
        let items = names.into_iter().map(|n| (n.clone(), n)).collect();
        let marked: Vec<Choice> = self
            .excluded
            .iter()
            .map(|t| (t.clone(), t.clone()))
            .collect();
        let title = "Hide PRs requested only from";
        self.multi_picker(title, "None", items, &marked, Pick::Exclude);
    }

    fn exclude(&mut self, teams: Vec<String>) {
        if let Some(path) = &self.config
            && let Err(e) = config::save(path, &teams)
        {
            let text = format!("{}: {e}", path.display());
            return self.error(text);
        }
        self.excluded = teams;
        if self.list().filters.iter().any(|f| f.1 == api::MY_TEAMS) {
            self.reset();
        } else {
            self.info("saved; applies to the filter Review requested: my teams");
        }
    }

    /// A picker for several choices, with an "All" row. Tabs without choices
    /// have none to pick.
    fn multi_picker(
        &mut self,
        title: &str,
        all: &str,
        items: Vec<Choice>,
        marked: &[Choice],
        pick: Pick,
    ) {
        if items.is_empty() {
            return;
        }
        let mut all = vec![(all.to_string(), String::new())];
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

    /// Letters run an action at once; arrows and Enter choose one.
    fn menu_key(&mut self, sel: usize, key: KeyEvent) {
        let items = keys::menu(self.tab);
        let Some(&(group, chosen)) = items.get(sel) else {
            return;
        };
        let last = items.len().saturating_sub(1);
        let first_of = |g: usize| items.iter().position(|i| i.0 == g);
        let to = match key.code {
            KeyCode::Esc | KeyCode::Char('.') => return,
            KeyCode::Enter => return self.act(chosen.act),
            KeyCode::Char(c) => match keys::lookup(self.tab, c) {
                Some(act) => return self.act(act),
                None => sel,
            },
            KeyCode::Right => (sel + 1).min(last),
            KeyCode::Left => sel.saturating_sub(1),
            KeyCode::Down => first_of(group + 1).unwrap_or(sel),
            KeyCode::Up => group.checked_sub(1).and_then(first_of).unwrap_or(sel),
            _ => sel,
        };
        self.modal = Some(Modal::Menu(to));
    }

    /// Whether an action makes sense for the selected item. Only a hint for the
    /// labels: GitHub has the last word.
    pub fn applies(&self, act: Act) -> bool {
        let anytime = matches!(
            act,
            Act::Org
                | Act::Repo
                | Act::Filter
                | Act::State
                | Act::Exclude
                | Act::Find
                | Act::More
                | Act::Refresh
                | Act::Menu
                | Act::Help
                | Act::Message
                | Act::Quit
                | Act::New
                | Act::Trigger
                | Act::Bots
        );
        if anytime {
            return true;
        }
        if act == Act::Seen {
            return !self.unread.is_empty();
        }
        let Some(item) = self.current() else {
            return false;
        };
        let state = item.state();
        let mine = self.is_mine(item);
        match act {
            Act::Raw => self.source.markdown,
            Act::NextThread | Act::PrevThread => self.doc.has_threads(),
            Act::Resolved => self.source.threads.iter().any(|t| t.resolved),
            Act::Lines => self.doc.has_files(),
            Act::AiPost => self.source.ai.as_ref().is_some_and(|r| r.accepted() > 0),
            Act::Reviewers => mine && matches!(state, "open" | "draft"),
            Act::Title | Act::Description | Act::Tasks => mine,
            Act::Ready => mine && state == "draft",
            // GitHub does not let authors review their own pull request.
            Act::Approve | Act::RequestChanges => {
                matches!(state, "open" | "draft") && (!mine || self.repos.is_none())
            }
            Act::Merge => state == "open",
            Act::Toggle => state != "merged",
            Act::Publish => state == "draft",
            Act::Cancel => matches!(item, Item::Run(r) if r.conclusion.is_none()),
            Act::Log | Act::Rerun => matches!(item, Item::Run(r) if r.conclusion.is_some()),
            Act::FailedLog | Act::RerunFailed => {
                matches!(state, "failure" | "timed_out" | "startup_failure")
            }
            _ => true,
        }
    }

    /// The modal was taken out; put it back to keep it open.
    fn modal_key(&mut self, modal: Modal, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match modal {
            Modal::Help | Modal::Message(_) => {}
            Modal::Menu(sel) => self.menu_key(sel, key),
            Modal::Confirm(cmd) => match key.code {
                KeyCode::Char('y') => self.execute(cmd, None),
                KeyCode::Char('n') | KeyCode::Esc => {}
                _ => self.modal = Some(Modal::Confirm(cmd)),
            },
            Modal::Form(mut form) => {
                if key.code == KeyCode::Esc {
                    return;
                }
                if ctrl && key.code == KeyCode::Char('s') && form.act == Act::AiEdit {
                    // Stays here; nothing is sent.
                    match self.ai_edited(&form) {
                        Ok(()) => return,
                        Err(e) => form.error = Some(e),
                    }
                } else if ctrl && key.code == KeyCode::Char('s') {
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
                bots: list.bots,
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
                self.save_view();
            }
            Pick::Scope => {
                self.scope = chosen;
                self.fixed = false; // chosen here, so worth keeping
                self.clear_lists();
                self.save_view();
            }
            Pick::Filter => {
                self.list_mut().filters = chosen;
                self.reset();
                self.save_view();
            }
            Pick::State => {
                self.list_mut().states = chosen;
                self.reset();
                self.save_view();
            }
            Pick::Exclude => self.exclude(chosen.into_iter().map(|c| c.1).collect()),
            Pick::Reviewers {
                repo,
                number,
                requested,
            } => {
                let chosen: Vec<String> = chosen.into_iter().map(|c| c.1).collect();
                match api::request_reviews(&repo, number, &requested, &chosen) {
                    Some(cmd) => self.modal = Some(Modal::Confirm(cmd)),
                    None => self.info("reviewers unchanged"),
                }
            }
            Pick::Tasks { repo, number, body } => {
                let ticked: Vec<usize> = chosen.iter().filter_map(|c| c.1.parse().ok()).collect();
                let new = tasks::apply(&body, &ticked);
                if new == body {
                    return self.info("checkboxes unchanged");
                }
                let done = tasks::find(&body).into_iter().filter(|t| t.done);
                let cleared = done.filter(|t| !ticked.contains(&t.mark)).count();
                let mut cmd = api::edit_body(&repo, number, &body, new);
                cmd.label = format!("Update checkboxes of {repo}#{number}");
                if cleared > 0 {
                    // One key clears them all, so ask.
                    cmd.label += &format!(" (unticks {cleared})");
                    self.modal = Some(Modal::Confirm(cmd));
                } else {
                    self.execute(cmd, None);
                }
            }
            Pick::Target(_) | Pick::Workflow(_) | Pick::AiTool | Pick::AiAgain => {}
        }
    }

    fn picked(&mut self, pick: Pick, value: String) {
        match pick {
            Pick::Org
            | Pick::Scope
            | Pick::Filter
            | Pick::State
            | Pick::Exclude
            | Pick::Reviewers { .. }
            | Pick::Tasks { .. } => {}
            Pick::AiAgain => match value.as_str() {
                "run" => self.ai_start(),
                _ => self.ai_choose(),
            },
            Pick::AiTool => {
                let tool = Tool::ALL.into_iter().find(|t| t.name() == value);
                let Some(tool) = tool else {
                    return self.error("this tool cannot be used");
                };
                self.ai.tool = Some(tool);
                if let Some(path) = &self.ai_path
                    && let Err(e) = config::save_ai(path, &self.ai)
                {
                    let text = format!("{}: {e}", path.display());
                    self.error(text);
                }
                self.ai_run(tool);
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
    if cfg!(test) {
        return Ok(());
    }
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

/// Doors for tests that act like workers.
#[cfg(test)]
impl App {
    pub fn feed(&mut self, tab: Tab, append: bool, result: Result<Page>) {
        let job = self.lists[tab as usize].job;
        self.listed(tab, job, append, result);
    }

    pub fn feed_ai(&mut self, text: &str, count: usize) {
        let (job, oid) = (self.ai_job, self.source.oid.clone());
        let item = self.doc_id.clone();
        let findings = (0..count).map(|n| ai::Finding {
            path: ["f", "other"][n % 2].into(),
            line: (n != 1).then_some(n as u32 + 1),
            side: ["RIGHT", "LEFT"][n / 2 % 2].into(),
            severity: "nit".into(),
            title: text.into(),
            body: text.into(),
            accepted: n == 0,
        });
        self.handle(Msg::Ai {
            job,
            item,
            result: Ok(Review {
                oid,
                findings: findings.collect(),
                left_out: vec![text.into(); count],
                ..Review::default()
            }),
        });
    }

    pub fn feed_doc(&mut self, refresh: bool, result: Result<Source>) {
        let job = self.doc_job;
        self.handle(Msg::Doc {
            job,
            bottom: false,
            refresh,
            result,
        });
    }

    pub fn feed_pick(&mut self, people: Candidates, body: &str, ready: Readiness, which: u32) {
        let job = self.pick_job;
        let Some(item) = self.list().selected().cloned() else {
            return;
        };
        let (repo, number) = ("o/r".to_string(), 7);
        self.handle(match which {
            0 => Msg::Reviewers {
                job,
                repo,
                number,
                result: Ok(people),
            },
            1 => Msg::Merge {
                job,
                item: Box::new(item),
                result: Ok(ready),
            },
            _ => Msg::Text {
                job,
                act: [Act::Title, Act::Description, Act::Tasks][body.len() % 3],
                item: Box::new(item),
                result: Ok(PrText {
                    mine: true,
                    title: body.into(),
                    body: format!("- [ ] {body}"),
                }),
            },
        });
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
            reload: false,
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

    #[test]
    fn choosing_a_reviewer_keeps_the_others_and_asks() {
        let candidates = || Candidates {
            mine: true,
            requested: vec!["o/team".into()],
            people: vec![("al".into(), "al".into()), ("bo".into(), "bo".into())],
        };
        let label = |app: &App| match &app.modal {
            Some(Modal::Confirm(cmd)) => cmd.label.clone(),
            _ => String::new(),
        };
        let mut app = app();
        app.pick_reviewers("o/r".into(), 7, candidates());
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        assert_eq!(label(&app), "o/r#7: request review from al");
        press(&mut app, KeyCode::Char('n'));
        app.pick_reviewers("o/r".into(), 7, candidates());
        press(&mut app, KeyCode::Char(' '));
        press(&mut app, KeyCode::Enter);
        assert_eq!(label(&app), "o/r#7: withdraw request to o/team");
        assert!(app.busy.is_none());
    }

    #[test]
    fn reviewers_only_for_my_pull_requests() {
        let mut app = app();
        app.listed(Tab::Prs, 0, false, page(&[1]));
        app.repos = Some(Repos {
            login: "me".into(),
            ..Repos::default()
        });
        press(&mut app, KeyCode::Char('v'));
        assert!(app.busy.is_none());
        assert!(matches!(app.status, Some(Status::Error(_))));
    }

    #[test]
    fn excluded_teams_are_saved_and_can_be_cleared() {
        let dir = std::env::temp_dir().join(format!("ghdeck-app-{}", std::process::id()));
        let path = dir.join("excluded-teams");
        let mut app = app();
        app.lists[0].filters.clear(); // so nothing is reloaded
        app.teams_ready = true;
        app.config = Some(path.clone());
        app.repos = Some(Repos {
            teams: vec!["o/all".into(), "o/mine".into()],
            ..Repos::default()
        });
        for code in [
            KeyCode::Char('t'),
            KeyCode::Down,
            KeyCode::Char(' '),
            KeyCode::Enter,
        ] {
            press(&mut app, code);
        }
        assert_eq!(app.excluded, ["o/all"]);
        assert_eq!(config::load(&path).unwrap(), ["o/all"]);
        press(&mut app, KeyCode::Char('t'));
        app.key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL));
        press(&mut app, KeyCode::Enter);
        assert!(config::load(&path).unwrap().is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn default_filters_wait_for_the_teams() {
        let mut app = app();
        app.load(Tab::Prs, false);
        assert!(app.lists[0].waiting && app.lists[0].loading);
        app.load(Tab::Runs, false); // has no team filter
        assert!(!app.lists[3].waiting);
        app.handle(Msg::Repos(Err(GhError("offline".into()))));
        assert!(!app.lists[0].waiting && !app.lists[0].loading);
        app.load(Tab::Prs, false);
        app.handle(Msg::Teams(Err(GhError("no scope".into()))));
        assert!(!app.lists[0].waiting && app.lists[0].loading);
    }

    #[test]
    fn last_message_can_be_read_again() {
        let mut app = app();
        app.error("a long error");
        press(&mut app, KeyCode::Char('j')); // leaves the header
        assert!(app.status.is_none());
        press(&mut app, KeyCode::Char('!'));
        assert!(matches!(&app.modal, Some(Modal::Message(t)) if t == "a long error"));
        press(&mut app, KeyCode::Esc);
        assert!(app.modal.is_none());
    }

    #[test]
    fn ticking_keeps_other_marks_and_unticking_asks() {
        let body = "- [x] a\n- [ ] b\n".to_string();
        let mut app = app();
        app.pick_tasks("o/r".into(), 7, body.clone());
        press(&mut app, KeyCode::Char(' ')); // untick a
        press(&mut app, KeyCode::Enter);
        let Some(Modal::Confirm(cmd)) = app.modal.take() else {
            panic!("no confirmation");
        };
        assert_eq!(cmd.stdin.as_deref(), Some("- [ ] a\n- [ ] b\n"));
        assert!(cmd.guard.is_some_and(|g| g.expected == body));
        app.pick_tasks("o/r".into(), 7, body);
        press(&mut app, KeyCode::Down);
        let Some(Modal::Picker(p)) = &app.modal else {
            panic!("no picker");
        };
        assert_eq!(p.chosen().unwrap().len(), 2);
        app.modal = None;
        app.pick_tasks("o/r".into(), 7, "no boxes".into());
        assert!(app.modal.is_none());
    }

    #[test]
    fn menu_runs_by_letter_or_arrows() {
        let mut app = app();
        app.listed(Tab::Prs, 0, false, page(&[1]));
        for code in [KeyCode::Char('.'), KeyCode::Char('c')] {
            press(&mut app, code);
        }
        assert!(matches!(&app.modal, Some(Modal::Form(f)) if f.act == Act::Comment));
        press(&mut app, KeyCode::Esc);
        // Down to Review, right to Request changes.
        for code in [
            KeyCode::Char('.'),
            KeyCode::Down,
            KeyCode::Right,
            KeyCode::Enter,
        ] {
            press(&mut app, code);
        }
        assert!(matches!(&app.modal, Some(Modal::Form(f)) if f.act == Act::RequestChanges));
        press(&mut app, KeyCode::Esc);
        for code in [KeyCode::Char('.'), KeyCode::Char('Z')] {
            press(&mut app, code); // unknown letters keep it open
        }
        assert!(matches!(app.modal, Some(Modal::Menu(0))));
        press(&mut app, KeyCode::Esc);
        assert!(app.modal.is_none());
    }

    #[test]
    fn menu_arrows_stay_in_range() {
        let mut app = app();
        for tab in Tab::ALL {
            app.tab = tab;
            app.modal = Some(Modal::Menu(0));
            for code in [KeyCode::Up, KeyCode::Left, KeyCode::Down, KeyCode::Right] {
                for _ in 0..60 {
                    press(&mut app, code);
                }
                assert!(matches!(app.modal, Some(Modal::Menu(_))), "{tab:?}");
            }
        }
    }

    #[test]
    fn zoom_follows_the_document() {
        let mut app = app();
        press(&mut app, KeyCode::Char('z'));
        assert!(!app.zoom); // nothing selected
        app.listed(Tab::Prs, 0, false, page(&[1]));
        press(&mut app, KeyCode::Char('z'));
        assert!(app.zoom && app.detail_focus);
        press(&mut app, KeyCode::Char('z'));
        assert!(!app.zoom && app.detail_focus);
        press(&mut app, KeyCode::Char('z'));
        press(&mut app, KeyCode::Esc);
        assert!(!app.zoom && !app.detail_focus);
    }

    #[test]
    fn raw_toggle_keeps_the_item() {
        let mut app = app();
        app.listed(Tab::Prs, 0, false, page(&[1]));
        press(&mut app, KeyCode::Char('M'));
        assert!(app.raw && app.source.markdown);
        press(&mut app, KeyCode::Char('M'));
        assert!(!app.raw);
        assert!(app.applies(Act::Raw));
        app.show(Source::plain("log".into()));
        assert!(!app.applies(Act::Raw));
    }

    fn reviewing() -> App {
        let mut app = app();
        app.listed(Tab::Prs, 0, false, page(&[1]));
        app.detail_focus = true;
        app.doc_height = 40;
        let diff = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,2 +1,2 @@\n-a\n+b\n c\n";
        app.show(Source {
            diff: Some(diff.into()),
            oid: "abc".into(),
            threads: vec![review::Thread {
                id: "T".into(),
                path: "f".into(),
                line: Some(1),
                side: "RIGHT".into(),
                can_reply: true,
                can_toggle: true,
                total: 1,
                comments: vec![review::Comment {
                    id: Some(9),
                    body: "why".into(),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            ..Source::default()
        });
        app.doc.layout(80);
        app
    }

    #[test]
    fn line_mode_comments_and_keeps_other_keys_away() {
        let mut app = reviewing();
        press(&mut app, KeyCode::Char('l'));
        press(&mut app, KeyCode::Char('m'));
        assert!(app.modal.is_none() && app.busy.is_none());
        press(&mut app, KeyCode::Char('j')); // +b
        press(&mut app, KeyCode::Enter);
        let Some(Modal::Form(mut form)) = app.modal.take() else {
            panic!("no form");
        };
        assert_eq!(form.title, "Comment on f:1");
        form.paste("hi");
        let sent = api::submit(&form).unwrap().stdin.unwrap();
        for part in [
            "\"commit_id\":\"abc\"",
            "\"line\":1",
            "\"side\":\"RIGHT\"",
            "\"body\":\"hi\"",
        ] {
            assert!(sent.contains(part), "{part} missing in {sent}");
        }
        press(&mut app, KeyCode::Esc);
        assert!(app.doc.cursor.is_none() && app.detail_focus);
    }

    #[test]
    fn discussions_can_be_answered_and_resolved() {
        let mut app = reviewing();
        press(&mut app, KeyCode::Char('u'));
        press(&mut app, KeyCode::Enter);
        assert!(
            matches!(&app.modal, Some(Modal::Form(f)) if f.act == Act::Reply && f.extra == "9")
        );
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('x'));
        assert!(matches!(&app.modal, Some(Modal::Confirm(c)) if c.label.starts_with("Resolve")));
        press(&mut app, KeyCode::Char('n'));
        app.source.threads[0].can_reply = false;
        app.source.threads[0].can_toggle = false;
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('x'));
        assert!(app.modal.is_none());
    }

    #[test]
    fn resolved_toggle_returns_to_the_discussion() {
        let mut app = reviewing();
        app.source.threads[0].resolved = true;
        app.toggle_resolved();
        press(&mut app, KeyCode::Char('u'));
        press(&mut app, KeyCode::Char('O'));
        assert_eq!(app.doc.thread_at_cursor(), Some(0));
        assert!(!app.source.show_resolved);
    }

    #[test]
    fn merge_needs_the_reviewed_commit_and_no_conflicts() {
        let mut app = app();
        app.listed(Tab::Prs, 0, false, page(&[1]));
        let item = app.list().selected().unwrap().clone();
        let ready = |oid: &str, conflicts: bool| Readiness {
            oid: oid.into(),
            conflicts,
            methods: vec!["Squash"],
            notes: vec![(false, "blocked".into())],
        };
        app.merge_form(item.clone(), ready("new", false));
        assert!(app.modal.is_none());
        app.merge_form(item.clone(), ready("", true));
        assert!(app.modal.is_none());
        app.merge_form(item, ready("", false));
        let Some(Modal::Form(form)) = &app.modal else {
            panic!("no form");
        };
        assert_eq!((form.chosen(0), form.notes.len()), ("Squash", 1));
    }

    #[test]
    fn view_is_restored_but_not_over_the_command_line() {
        let dir = std::env::temp_dir().join(format!("ghdeck-restore-{}", std::process::id()));
        let path = dir.join("view.json");
        let pick = |v: &str| vec![(v.to_string(), v.to_string())];
        let mut app = App::new(mpsc::channel().0, None);
        app.view_path = Some(path.clone());
        app.orgs = pick("o");
        app.scope = pick("o/kept");
        app.lists[0].states.clear();
        app.save_view();
        let saved = config::load_view(&path).unwrap().unwrap();

        let mut next = App::new(mpsc::channel().0, None);
        next.restore(saved.clone());
        assert_eq!((next.orgs, next.scope), (pick("o"), pick("o/kept")));
        assert!(next.lists[0].states.is_empty());

        let mut fixed = App::new(mpsc::channel().0, Some("a/b".into()));
        fixed.restore(saved);
        fixed.view_path = Some(path.clone());
        assert_eq!(fixed.scope, pick("a/b"));
        fixed.save_view();
        let again = config::load_view(&path).unwrap().unwrap();
        assert_eq!(again.repos, pick("o/kept"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn line_mode_survives_a_reload() {
        let mut app = reviewing();
        let again = |app: &mut App| {
            let mut source = Source {
                diff: app.source.diff.clone(),
                threads: app.source.threads.clone(),
                ..Source::default()
            };
            // A new discussion came first.
            source.threads.insert(0, review::Thread::default());
            let job = app.doc_job;
            app.handle(Msg::Doc {
                job,
                bottom: false,
                refresh: true,
                result: Ok(source),
            });
        };
        press(&mut app, KeyCode::Char('u'));
        again(&mut app);
        let n = app.doc.thread_at_cursor().unwrap();
        assert_eq!(app.source.threads[n].id, "T");
        press(&mut app, KeyCode::Char('x'));
        assert!(matches!(&app.modal, Some(Modal::Confirm(c)) if c.label.starts_with("Resolve")));
        press(&mut app, KeyCode::Char('n'));
        press(&mut app, KeyCode::Char('j')); // the line after the discussion
        let before = app.doc.mark();
        again(&mut app);
        assert_eq!(app.doc.mark(), before);
    }

    fn finding(title: &str, line: Option<u32>) -> ai::Finding {
        ai::Finding {
            path: "f".into(),
            line,
            side: "RIGHT".into(),
            severity: "major".into(),
            title: title.into(),
            body: "b".into(),
            accepted: false,
        }
    }

    fn with_findings() -> App {
        let mut app = reviewing();
        app.ai_job = 5;
        app.ai_running = Some("x".into());
        app.handle(Msg::Ai {
            job: 5,
            item: "o/r#1".into(),
            result: Ok(Review {
                oid: "abc".into(),
                tool: "X".into(),
                findings: vec![
                    finding("one", Some(1)),
                    finding("two", Some(2)),
                    finding("three", None),
                ],
                ..Review::default()
            }),
        });
        app
    }

    #[test]
    fn findings_are_accepted_edited_discarded_and_posted() {
        let mut app = with_findings();
        assert!(app.ai_running.is_none());
        press(&mut app, KeyCode::Char('P'));
        assert!(app.modal.is_none(), "nothing is accepted yet");
        press(&mut app, KeyCode::Char('A'));
        let at = app.doc.finding_at_cursor().unwrap();
        press(&mut app, KeyCode::Char(' '));
        assert!(app.reviews["o/r#1"].findings[at].accepted);
        assert_eq!(app.doc.finding_at_cursor(), Some(at), "the cursor stays");
        press(&mut app, KeyCode::Char('u'));
        while app.doc.finding_at_cursor().is_none() {
            press(&mut app, KeyCode::Char('u'));
        }
        let next = app.doc.finding_at_cursor().unwrap();
        press(&mut app, KeyCode::Char('e'));
        press(&mut app, KeyCode::Char('!'));
        app.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(
            app.modal.is_none() && app.busy.is_none(),
            "editing sends nothing"
        );
        assert_eq!(app.reviews["o/r#1"].findings[next].body, "b!");
        assert_eq!(app.reviews["o/r#1"].accepted(), 2);
        press(&mut app, KeyCode::Char('u'));
        while app
            .doc
            .finding_at_cursor()
            .is_none_or(|n| n == at || n == next)
        {
            press(&mut app, KeyCode::Char('u'));
        }
        press(&mut app, KeyCode::Char('d'));
        assert_eq!(app.reviews["o/r#1"].findings.len(), 2);
        press(&mut app, KeyCode::Char('P'));
        assert!(matches!(&app.modal, Some(Modal::Form(f)) if f.act == Act::AiPost));
        app.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert!(app.busy.is_some());
        let Some(Modal::Form(_)) = app.modal else {
            let form = Form::new(
                Tab::Prs,
                Act::AiPost,
                "o/r",
                app.list().selected().cloned(),
                "",
            );
            app.handle(Msg::Done {
                tab: Tab::Prs,
                reload: true,
                label: "x".into(),
                form: form.map(Box::new),
                result: Ok(String::new()),
            });
            assert!(
                app.reviews["o/r#1"].findings.is_empty(),
                "posted ones are gone"
            );
            return;
        };
        panic!("the form is still open");
    }

    #[test]
    fn outdated_findings_are_not_posted() {
        let mut app = with_findings();
        app.reviews.get_mut("o/r#1").unwrap().findings[0].accepted = true;
        let mut newer = Source {
            diff: app.source.diff.clone(),
            oid: "newer".into(),
            ..Source::default()
        };
        app.show(std::mem::take(&mut newer));
        app.doc.layout(80);
        assert!(app.source.ai.as_ref().unwrap().outdated);
        press(&mut app, KeyCode::Char('P'));
        assert!(app.modal.is_none());
    }

    #[test]
    fn ai_respects_blocking_and_can_be_stopped() {
        let mut app = reviewing();
        app.ai.tool = Some(Tool::Claude);
        app.ai.blocked = vec!["o".into()];
        press(&mut app, KeyCode::Char('A'));
        assert!(app.ai_running.is_none());
        app.ai.blocked.clear();
        press(&mut app, KeyCode::Char('A'));
        assert!(app.ai_running.is_some());
        let job = app.ai_job;
        press(&mut app, KeyCode::Char('A'));
        assert!(app.ai_running.is_none());
        app.handle(Msg::Ai {
            job,
            item: "o/r#1".into(),
            result: Ok(Review::default()),
        });
        assert!(app.reviews.is_empty(), "a late answer is ignored");
    }

    #[test]
    fn unusable_tools_cannot_be_chosen() {
        let mut app = reviewing();
        app.handle(Msg::AiTools(vec![
            (Tool::Claude, "sends".into(), Some("not installed".into())),
            (Tool::Custom, "gets".into(), None),
        ]));
        press(&mut app, KeyCode::Enter);
        assert!(app.ai.tool.is_none() && app.ai_running.is_none());
        app.handle(Msg::AiTools(vec![(Tool::Custom, "gets".into(), None)]));
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.ai.tool, Some(Tool::Custom));
        assert!(app.ai_running.is_some());
    }

    #[test]
    fn discarding_moves_to_the_next_finding_on_screen() {
        let mut app = with_findings();
        // On screen: three (no line), one, two.
        press(&mut app, KeyCode::Char('A'));
        app.doc.go_to_finding(Some(2), 40);
        press(&mut app, KeyCode::Char('d'));
        let title = |app: &App| {
            let n = app.doc.finding_at_cursor().unwrap();
            app.reviews["o/r#1"].findings[n].title.clone()
        };
        assert_eq!(title(&app), "one");
        press(&mut app, KeyCode::Char('d'));
        assert_eq!(title(&app), "two");
        press(&mut app, KeyCode::Char('d'));
        assert!(app.doc.finding_at_cursor().is_none());
    }

    #[test]
    fn a_range_starts_only_on_code() {
        let mut app = reviewing();
        press(&mut app, KeyCode::Char('u'));
        press(&mut app, KeyCode::Char(' '));
        assert!(app.doc.anchor.is_none());
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char(' '));
        assert!(app.doc.anchor.is_some());
    }

    #[test]
    fn one_press_is_enough_before_the_changes_are_loaded() {
        let loaded = |diff: Option<&str>| Source {
            diff: diff.map(String::from),
            oid: "abc".into(),
            ..Source::default()
        };
        let diff = "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1 +1 @@\n-a\n+b\n";
        let mut app = app();
        app.listed(Tab::Prs, 0, false, page(&[1, 2]));
        app.ai.tool = Some(Tool::Claude);
        press(&mut app, KeyCode::Char('A'));
        assert!(app.ai_running.is_none() && app.doc_loading);
        app.feed_doc(false, Ok(loaded(Some(diff))));
        assert!(app.ai_running.take().is_some());
        // Another item, and Esc, forget it.
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('A'));
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Enter);
        app.feed_doc(false, Ok(loaded(Some(diff))));
        assert!(app.ai_running.is_none());
        // Without changes it ends with a message, not in a loop.
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('l'));
        app.feed_doc(false, Ok(loaded(None)));
        assert!(!app.doc_loading && app.doc.cursor.is_none());
        press(&mut app, KeyCode::Char('l'));
        app.feed_doc(false, Ok(loaded(Some(diff))));
        assert!(app.doc.cursor.is_some());
    }

    fn events(keys: &[(&str, u64, bool)]) -> Result<Page> {
        let items = keys.iter().map(|&(key, number, bot)| {
            let pr = page(&[number]).unwrap().items.remove(0);
            Item::Event(crate::activity::Event {
                key: key.into(),
                text: format!("◆ al commented {key}"),
                at: String::new(),
                bot,
                comment: (key == "b").then_some(9),
                pr: Box::new(pr),
            })
        });
        Ok(Page {
            items: items.collect(),
            ..Page::default()
        })
    }

    #[test]
    fn activity_is_seen_by_opening_or_all_at_once() {
        let dir = std::env::temp_dir().join(format!("ghdeck-seen-{}", std::process::id()));
        let path = dir.join("seen.json");
        let mut app = app();
        app.seen_path = Some(path.clone());
        app.listed(Tab::Prs, 0, false, page(&[1, 2]));
        app.feed(
            Tab::Activity,
            false,
            events(&[
                ("a", 1, false),
                ("b", 1, false),
                ("c", 2, false),
                ("bot", 2, true),
            ]),
        );
        assert_eq!(app.unread.len(), 3, "bots are hidden");
        assert!(app.unread_prs.contains("o/r#1"));
        press(&mut app, KeyCode::Enter); // opens #1 in the PR list
        assert_eq!(app.unread.len(), 1);
        assert!(!app.unread_prs.contains("o/r#1"));
        press(&mut app, KeyCode::Char('5'));
        assert_eq!(app.list().view.len(), 3);
        press(&mut app, KeyCode::Char('B'));
        assert_eq!((app.list().view.len(), app.unread.len()), (4, 2));
        press(&mut app, KeyCode::Char('S'));
        assert!(app.unread.is_empty());
        assert_eq!(config::load_seen(&path).unwrap().len(), 4);
        // Seen ones stay seen, new ones are new.
        app.feed(
            Tab::Activity,
            false,
            events(&[("new", 1, false), ("a", 1, false)]),
        );
        assert_eq!(app.unread.len(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_event_acts_on_its_pull_request_and_opens_the_discussion() {
        let mut app = app();
        app.feed(
            Tab::Activity,
            false,
            events(&[("a", 7, false), ("b", 8, false)]),
        );
        press(&mut app, KeyCode::Char('5'));
        press(&mut app, KeyCode::Char('j'));
        assert_eq!(app.current().unwrap().id(), "o/r#8");
        press(&mut app, KeyCode::Char('c'));
        let Some(Modal::Form(form)) = app.modal.take() else {
            panic!("no form");
        };
        assert_eq!(form.title, "Comment on o/r#8");
        press(&mut app, KeyCode::Enter);
        assert!(app.detail_focus);
        let source = reviewing().source;
        app.feed_doc(false, Ok(source));
        assert_eq!(app.doc.thread_at_cursor(), Some(0));
    }

    #[test]
    fn activity_waits_for_login_and_teams() {
        let mut app = app(); // has a repository, so other lists do not wait
        app.load(Tab::Activity, false);
        assert!(app.lists[4].waiting);
        app.handle(Msg::Repos(Ok((Repos::default(), None))));
        assert!(app.lists[4].waiting);
        app.handle(Msg::Teams(Ok(Vec::new())));
        assert!(!app.lists[4].waiting && app.lists[4].loading);
    }
}
