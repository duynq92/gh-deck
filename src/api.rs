//! GitHub data: list queries, detail documents and mutating commands.
//! `--jq` trims responses inside `gh`, so only displayed fields reach us.

use crate::form::Form;
use crate::gh::{self, Cmd, GhError, Keep, Result};
use crate::keys::{Act, Tab};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use std::thread;

const PAGE: usize = 50;
const RUN_REPOS: usize = 12;
const PARALLEL: usize = 4;
const ALL_REPOS: &str = "OWNER,COLLABORATOR,ORGANIZATION_MEMBER";

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Ticket {
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub state: String,
    pub author: Option<String>,
    pub updated: String,
    pub url: String,
    pub body: String,
    pub labels: Vec<String>,
    #[serde(default)]
    pub assignees: Vec<String>,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub head: String,
    #[serde(default)]
    pub base: String,
    #[serde(default)]
    pub oid: String,
    pub review: Option<String>,
    pub checks: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Release {
    pub repo: String,
    pub tag: String,
    pub name: Option<String>,
    pub draft: bool,
    pub prerelease: bool,
    pub updated: String,
    pub url: String,
    pub body: Option<String>,
    pub author: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Run {
    pub repo: String,
    pub id: u64,
    pub name: Option<String>,
    pub title: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub branch: Option<String>,
    pub event: String,
    pub updated: String,
    pub url: String,
    pub actor: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Item {
    Ticket(Ticket),
    Release(Release),
    Run(Run),
}

impl Item {
    pub fn repo(&self) -> &str {
        match self {
            Item::Ticket(t) => &t.repo,
            Item::Release(r) => &r.repo,
            Item::Run(r) => &r.repo,
        }
    }

    pub fn url(&self) -> &str {
        match self {
            Item::Ticket(t) => &t.url,
            Item::Release(r) => &r.url,
            Item::Run(r) => &r.url,
        }
    }

    pub fn updated(&self) -> &str {
        match self {
            Item::Ticket(t) => &t.updated,
            Item::Release(r) => &r.updated,
            Item::Run(r) => &r.updated,
        }
    }

    /// Short identifier, e.g. `owner/name#12`.
    pub fn id(&self) -> String {
        match self {
            Item::Ticket(t) => format!("{}#{}", t.repo, t.number),
            Item::Release(r) => format!("{} {}", r.repo, r.tag),
            Item::Run(r) => format!("{} run {}", r.repo, r.id),
        }
    }

    pub fn title(&self) -> &str {
        match self {
            Item::Ticket(t) => &t.title,
            Item::Release(r) => match r.name.as_deref() {
                Some(n) if !n.is_empty() => n,
                _ => &r.tag,
            },
            Item::Run(r) => &r.title,
        }
    }

    pub fn state(&self) -> &str {
        match self {
            Item::Ticket(t) if t.draft && t.state == "OPEN" => "draft",
            Item::Ticket(t) => match t.state.as_str() {
                "OPEN" => "open",
                "MERGED" => "merged",
                _ => "closed",
            },
            Item::Release(r) if r.draft => "draft",
            Item::Release(r) if r.prerelease => "pre",
            Item::Release(_) => "released",
            Item::Run(r) => r.conclusion.as_deref().unwrap_or(&r.status),
        }
    }

    /// Text used by the local find filter.
    pub fn matches(&self, needle: &str) -> bool {
        let author = match self {
            Item::Ticket(t) => t.author.as_deref(),
            Item::Release(r) => r.author.as_deref(),
            Item::Run(r) => r.name.as_deref(),
        };
        format!(
            "{} {} {} {}",
            self.id(),
            self.title(),
            self.state(),
            author.unwrap_or("")
        )
        .to_lowercase()
        .contains(needle)
    }

    /// Instant preview, built from data the list already holds.
    pub fn preview(&self) -> String {
        let who = |a: &Option<String>| a.clone().unwrap_or_else(|| "ghost".into());
        match self {
            Item::Ticket(t) => {
                let mut s = format!(
                    "{}\n{} · {} · @{} · updated {}\n",
                    t.title,
                    self.id(),
                    self.state(),
                    who(&t.author),
                    date(&t.updated)
                );
                if !t.head.is_empty() {
                    s += &format!("{} ← {}\n", t.base, t.head);
                }
                for (name, value) in [("Review", &t.review), ("Checks", &t.checks)] {
                    if let Some(v) = value {
                        s += &format!("{name}: {}\n", v.to_lowercase().replace('_', " "));
                    }
                }
                for (name, list) in [("Labels", &t.labels), ("Assignees", &t.assignees)] {
                    if !list.is_empty() {
                        s += &format!("{name}: {}\n", list.join(", "));
                    }
                }
                s + "\n"
                    + if t.body.is_empty() {
                        "(no description)"
                    } else {
                        &t.body
                    }
            }
            Item::Release(r) => format!(
                "{}\n{} · {} · @{} · {}\n\n{}",
                self.title(),
                self.id(),
                self.state(),
                who(&r.author),
                date(&r.updated),
                r.body.as_deref().unwrap_or("(no notes)")
            ),
            Item::Run(r) => format!(
                "{}\n{} · {}\nWorkflow: {}\nBranch: {}\nEvent: {} by @{}\nUpdated: {}",
                r.title,
                self.id(),
                self.state(),
                r.name.as_deref().unwrap_or("-"),
                r.branch.as_deref().unwrap_or("-"),
                r.event,
                who(&r.actor),
                date(&r.updated)
            ),
        }
    }
}

/// `2026-01-02T03:04:05Z` -> `2026-01-02 03:04`
pub fn date(iso: &str) -> String {
    iso.get(..16).unwrap_or(iso).replace('T', " ")
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Repos {
    pub login: String,
    /// Most recently pushed first.
    pub names: Vec<String>,
}

#[derive(Debug, Default)]
pub struct Page {
    pub items: Vec<Item>,
    pub total: usize,
    pub cursor: Option<String>,
    pub warning: Option<String>,
}

/// What a list should show.
#[derive(Debug, Clone)]
pub struct Query {
    pub tab: Tab,
    pub scope: Option<String>,
    pub filter: usize,
    pub state: usize,
    pub cursor: Option<String>,
}

pub fn filters(tab: Tab) -> &'static [(&'static str, &'static str)] {
    match tab {
        Tab::Prs => &[
            ("My repos", ""),
            ("Involves me", "involves:@me"),
            ("Created by me", "author:@me"),
            ("Review requested", "review-requested:@me"),
        ],
        Tab::Issues => &[
            ("My repos", ""),
            ("Involves me", "involves:@me"),
            ("Created by me", "author:@me"),
            ("Assigned to me", "assignee:@me"),
        ],
        Tab::Releases | Tab::Runs => &[("My repos", "OWNER"), ("All accessible", ALL_REPOS)],
    }
}

pub fn states(tab: Tab) -> &'static [(&'static str, &'static str)] {
    match tab {
        Tab::Prs => &[
            ("Open", "is:open"),
            ("Merged", "is:merged"),
            ("Closed", "is:closed is:unmerged"),
            ("All", ""),
        ],
        Tab::Issues => &[("Open", "is:open"), ("Closed", "is:closed"), ("All", "")],
        Tab::Releases => &[("All", "")],
        Tab::Runs => &[
            ("All", ""),
            ("Failed", "failure"),
            ("Running", "in_progress"),
            ("Queued", "queued"),
        ],
    }
}

pub fn valid_repo(name: &str) -> bool {
    let part = |p: &str| {
        !p.is_empty()
            && p != "."
            && p != ".."
            && !p.starts_with('-')
            && p.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    };
    matches!(name.split_once('/'), Some((o, n)) if part(o) && part(n))
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn fetch<T: DeserializeOwned>(list: &[&str]) -> Result<T> {
    let text = gh::run(&args(list), None, gh::JSON)?;
    serde_json::from_str(&text).map_err(|e| GhError(format!("unexpected gh response: {e}")))
}

pub fn repos() -> Result<Repos> {
    fetch(&[
        "api",
        "graphql",
        "-f",
        "query={viewer{login repositories(first:100,\
         affiliations:[OWNER,COLLABORATOR,ORGANIZATION_MEMBER],\
         ownerAffiliations:[OWNER,COLLABORATOR,ORGANIZATION_MEMBER],\
         orderBy:{field:PUSHED_AT,direction:DESC}){nodes{nameWithOwner isArchived}}}}",
        "--jq",
        ".data.viewer|{login,names:[.repositories.nodes[]|select(.isArchived|not)|.nameWithOwner]}",
    ])
}

pub fn list(q: &Query, repos: &Repos) -> Result<Page> {
    match q.tab {
        Tab::Prs | Tab::Issues => tickets(q, repos),
        Tab::Releases => releases(q),
        Tab::Runs => runs(q, repos),
    }
}

fn search_query(q: &Query, login: &str) -> String {
    let kind = if q.tab == Tab::Prs {
        "is:pr"
    } else {
        "is:issue"
    };
    let filter = filters(q.tab)[q.filter].1;
    let place = match &q.scope {
        Some(repo) => format!("repo:{repo}"),
        None if filter.is_empty() => format!("user:{login}"),
        None => String::new(),
    };
    [
        kind,
        &place,
        filter,
        states(q.tab)[q.state].1,
        "archived:false sort:updated-desc",
    ]
    .iter()
    .filter(|s| !s.is_empty())
    .copied()
    .collect::<Vec<_>>()
    .join(" ")
}

const TICKET: &str = "number title state updatedAt url body author{login} \
    repository{nameWithOwner} labels(first:20){nodes{name}} assignees(first:20){nodes{login}}";

fn tickets(q: &Query, repos: &Repos) -> Result<Page> {
    #[derive(Deserialize)]
    struct Out {
        total: usize,
        cursor: Option<String>,
        items: Vec<Ticket>,
    }
    let kind = if q.tab == Tab::Prs {
        format!(
            "... on PullRequest{{{TICKET} isDraft headRefName headRefOid baseRefName \
             reviewDecision commits(last:1){{nodes{{commit{{statusCheckRollup{{state}}}}}}}}}}"
        )
    } else {
        format!("... on Issue{{{TICKET}}}")
    };
    let query = format!(
        "query=query($q:String!,$after:String){{search(type:ISSUE,first:{PAGE},query:$q,\
         after:$after){{issueCount pageInfo{{hasNextPage endCursor}} nodes{{{kind}}}}}}}"
    );
    let search = format!("q={}", search_query(q, &repos.login));
    let after = q.cursor.as_ref().map(|c| format!("after={c}"));
    let mut list = vec!["api", "graphql", "-f", &query, "-f", &search];
    if let Some(after) = &after {
        list.extend(["-f", after]);
    }
    list.extend([
        "--jq",
        ".data.search|{total:.issueCount,\
         cursor:(if .pageInfo.hasNextPage then .pageInfo.endCursor else null end),\
         items:[.nodes[]|select(.number)|{repo:.repository.nameWithOwner,number,title,state,\
         updated:.updatedAt,url,body,author:.author.login,labels:[.labels.nodes[].name],\
         assignees:[.assignees.nodes[].login],draft:(.isDraft//false),head:(.headRefName//\"\"),\
         oid:(.headRefOid//\"\"),base:(.baseRefName//\"\"),review:.reviewDecision,\
         checks:.commits.nodes[0].commit.statusCheckRollup.state}]}",
    ]);
    let out: Out = fetch(&list)?;
    // The search index lags behind changes; trust the live state instead.
    let state = states(q.tab)[q.state].0.to_uppercase();
    let items = out
        .items
        .into_iter()
        .filter(|t| state == "ALL" || t.state == state)
        .map(Item::Ticket)
        .collect();
    Ok(Page {
        items,
        total: out.total,
        cursor: out.cursor,
        warning: None,
    })
}

fn releases(q: &Query) -> Result<Page> {
    let fields = |n: usize| {
        format!(
            "nameWithOwner releases(first:{n},orderBy:{{field:CREATED_AT,direction:DESC}}){{nodes{{\
             tagName name isDraft isPrerelease createdAt publishedAt url description author{{login}}}}}}"
        )
    };
    let map = "[.[]|.nameWithOwner as $r|.releases.nodes[]|{repo:$r,tag:.tagName,name,\
         draft:.isDraft,prerelease:.isPrerelease,updated:(.publishedAt//.createdAt),url,\
         body:.description,author:.author.login}]|sort_by(.updated)|reverse";
    let list: Vec<Release> = match &q.scope {
        Some(repo) => {
            let (owner, name) = split(repo)?;
            fetch(&[
                "api",
                "graphql",
                "-f",
                &format!(
                    "query=query($o:String!,$n:String!){{repository(owner:$o,name:$n){{{}}}}}",
                    fields(PAGE)
                ),
                "-f",
                &format!("o={owner}"),
                "-f",
                &format!("n={name}"),
                "--jq",
                &format!("[.data.repository]|{map}"),
            ])?
        }
        None => {
            let who = filters(q.tab)[q.filter].1;
            fetch(&[
                "api",
                "graphql",
                "-f",
                &format!(
                    "query={{viewer{{repositories(first:50,affiliations:[{who}],\
                     ownerAffiliations:[{who}],\
                     orderBy:{{field:PUSHED_AT,direction:DESC}}){{nodes{{{}}}}}}}}}",
                    fields(3)
                ),
                "--jq",
                &format!(".data.viewer.repositories.nodes|{map}"),
            ])?
        }
    };
    let total = list.len();
    Ok(Page {
        items: list.into_iter().map(Item::Release).collect(),
        total,
        ..Page::default()
    })
}

fn split(repo: &str) -> Result<(&str, &str)> {
    repo.split_once('/')
        .ok_or_else(|| GhError(format!("invalid repository {repo:?}")))
}

fn repo_runs(repo: &str, status: &str, count: usize) -> Result<Vec<Run>> {
    let status = match status {
        "" => String::new(),
        s => format!("&status={s}"),
    };
    fetch(&[
        "api",
        &format!("repos/{repo}/actions/runs?per_page={count}{status}"),
        "--jq",
        "[.workflow_runs[]|{repo:.repository.full_name,id,name,title:.display_title,status,\
         conclusion,branch:.head_branch,event,updated:.updated_at,url:.html_url,actor:.actor.login}]",
    ])
}

fn runs(q: &Query, repos: &Repos) -> Result<Page> {
    let status = states(q.tab)[q.state].1;
    let names: Vec<&str> = match &q.scope {
        Some(repo) => vec![repo],
        // REST has no cross-repository endpoint: one request per recent repository.
        None => repos
            .names
            .iter()
            .filter(|n| {
                filters(q.tab)[q.filter].1 == ALL_REPOS
                    || n.split('/').next() == Some(repos.login.as_str())
            })
            .take(RUN_REPOS)
            .map(String::as_str)
            .collect(),
    };
    let count = if q.scope.is_some() { PAGE } else { 5 };
    let mut all = Vec::new();
    let mut errors = Vec::new();
    for batch in names.chunks(PARALLEL) {
        let results: Vec<_> = thread::scope(|s| {
            let handles: Vec<_> = batch
                .iter()
                .map(|repo| s.spawn(move || repo_runs(repo, status, count)))
                .collect();
            handles
                .into_iter()
                .map(|h| {
                    h.join()
                        .unwrap_or_else(|_| Err(GhError("worker panicked".into())))
                })
                .collect()
        });
        for (repo, result) in batch.iter().zip(results) {
            match result {
                Ok(runs) => all.extend(runs),
                Err(e) => errors.push(format!("{repo}: {e}")),
            }
        }
    }
    if all.is_empty() && !errors.is_empty() {
        return Err(GhError(errors.join("; ")));
    }
    all.sort_by(|a, b| b.updated.cmp(&a.updated));
    Ok(Page {
        total: all.len(),
        items: all.into_iter().map(Item::Run).collect(),
        cursor: None,
        warning: (!errors.is_empty())
            .then(|| format!("{} repos failed: {}", errors.len(), errors.join("; "))),
    })
}

pub fn workflows(repo: &str) -> Result<Vec<(String, String)>> {
    #[derive(Deserialize)]
    struct Workflow {
        id: u64,
        name: String,
        path: String,
    }
    let list: Vec<Workflow> = fetch(&["workflow", "list", "-R", repo, "--json", "id,name,path"])?;
    Ok(list
        .into_iter()
        .map(|w| (format!("{}  ({})", w.name, w.path), w.id.to_string()))
        .collect())
}

/// Load a detail document. Returns its text.
pub fn detail(act: Act, item: &Item) -> Result<String> {
    let repo = item.repo();
    let get = |list: &[&str], keep: Keep| gh::run(&args(list), None, keep);
    match (act, item) {
        (Act::Info, Item::Ticket(t)) => {
            let n = t.number.to_string();
            let (kind, fields) = if t.head.is_empty() {
                ("issue", "comments")
            } else {
                ("pr", "comments,reviews")
            };
            let thread = get(
                &[
                    kind,
                    "view",
                    &n,
                    "-R",
                    repo,
                    "--json",
                    fields,
                    "--jq",
                    "[.comments[]|{a:.author.login,t:.createdAt,b:.body,k:\"commented\"}]\
                     +[.reviews[]?|select(.submittedAt)|{a:.author.login,t:.submittedAt,b:.body,\
                     k:(.state|ascii_downcase)}]|sort_by(.t)[]\
                     |\"── @\\(.a//\"ghost\") \\(.k) · \\(.t[:10])\\n\\(.b)\\n\"",
                ],
                gh::JSON,
            )?;
            let thread = if thread.trim().is_empty() {
                "── No comments yet"
            } else {
                &thread
            };
            Ok(format!("{}\n\n{thread}", item.preview()))
        }
        (Act::Info, Item::Release(r)) => {
            let flag = format!("--repo={repo}");
            let assets = get(
                &[
                    "release",
                    "view",
                    &flag,
                    "--json",
                    "assets",
                    "--jq",
                    ".assets[]|\"\\(.name)  \\(.size/1024|floor) KiB  \\(.downloadCount) downloads\"",
                    "--",
                    &r.tag,
                ],
                gh::JSON,
            )?;
            let assets = if assets.trim().is_empty() {
                "(none)"
            } else {
                &assets
            };
            Ok(format!("{}\n\n── Assets\n{assets}", item.preview()))
        }
        (Act::Info, Item::Run(r)) => {
            let jobs = get(
                &[
                    "run",
                    "view",
                    &r.id.to_string(),
                    "-R",
                    repo,
                    "--json",
                    "jobs",
                    "--jq",
                    "def st: if (.conclusion//\"\")==\"\" then .status else .conclusion end;\
                     .jobs[]|\"── \\(.name): \\(st)\",(.steps[]|\"   \\(st)  \\(.name)\")",
                ],
                gh::JSON,
            )?;
            Ok(format!("{}\n\n{jobs}", item.preview()))
        }
        (Act::Diff, Item::Ticket(t)) => get(
            &[
                "pr",
                "diff",
                &t.number.to_string(),
                "-R",
                repo,
                "--color=never",
            ],
            gh::DIFF,
        ),
        (Act::Checks, Item::Ticket(t)) => {
            let out = get(
                &[
                    "pr",
                    "view",
                    &t.number.to_string(),
                    "-R",
                    repo,
                    "--json",
                    "statusCheckRollup",
                    "--jq",
                    ".statusCheckRollup[]|\"\\(if (.conclusion//\"\")!=\"\" then .conclusion \
                     else (.state//.status) end|ascii_downcase)  \\(.name//.context)  \
                     \\(.workflowName//\"\")\"",
                ],
                gh::JSON,
            )?;
            Ok(if out.trim().is_empty() {
                "No checks reported".into()
            } else {
                out
            })
        }
        (Act::Log | Act::FailedLog, Item::Run(r)) => {
            let flag = if act == Act::Log {
                "--log"
            } else {
                "--log-failed"
            };
            get(
                &["run", "view", &r.id.to_string(), "-R", repo, flag],
                gh::LOG,
            )
        }
        _ => Err(GhError("nothing to show for this item".into())),
    }
}

/// `gh pr checkout` run in another repository's clone would fetch foreign
/// commits into it, so make sure the working directory matches first.
pub fn checkout(item: &Item) -> Result<()> {
    let Item::Ticket(t) = item else {
        return Err(GhError("not a pull request".into()));
    };
    let advice = || {
        GhError(format!(
            "start ghdeck inside a clone of {} to check out",
            t.repo
        ))
    };
    let here = gh::run(
        &args(&[
            "repo",
            "view",
            "--json",
            "nameWithOwner",
            "--jq",
            ".nameWithOwner",
        ]),
        None,
        gh::JSON,
    )
    .map_err(|_| advice())?;
    if !here.trim().eq_ignore_ascii_case(&t.repo) {
        return Err(advice());
    }
    gh::run(
        &args(&["pr", "checkout", &t.number.to_string(), "-R", &t.repo]),
        None,
        gh::JSON,
    )
    .map(drop)
}

fn csv(value: &str) -> String {
    if value.contains([',', '"']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

fn lines(text: &str) -> Vec<&str> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect()
}

/// Commands that need only a confirmation.
pub fn simple(act: Act, item: &Item) -> Option<Cmd> {
    let repo = item.repo();
    let (verb, list): (&str, Vec<String>) = match (act, item) {
        (Act::Toggle, Item::Ticket(t)) => {
            let kind = if t.head.is_empty() { "issue" } else { "pr" };
            let verb = match t.state.as_str() {
                "OPEN" => "close",
                "CLOSED" => "reopen",
                _ => return None,
            };
            let name = if verb == "close" { "Close" } else { "Reopen" };
            (name, args(&[kind, verb, &t.number.to_string(), "-R", repo]))
        }
        (Act::Publish, Item::Release(r)) if r.draft => (
            "Publish",
            args(&[
                "release",
                "edit",
                &format!("--repo={repo}"),
                "--draft=false",
                "--",
                &r.tag,
            ]),
        ),
        (Act::Delete, Item::Release(r)) => (
            "Permanently delete (the tag is kept)",
            args(&[
                "release",
                "delete",
                &format!("--repo={repo}"),
                "--yes",
                "--",
                &r.tag,
            ]),
        ),
        (Act::Rerun, Item::Run(r)) => (
            "Rerun all jobs of",
            args(&["run", "rerun", &r.id.to_string(), "-R", repo]),
        ),
        (Act::RerunFailed, Item::Run(r)) => (
            "Rerun failed jobs of",
            args(&["run", "rerun", &r.id.to_string(), "-R", repo, "--failed"]),
        ),
        (Act::Cancel, Item::Run(r)) => (
            "Cancel",
            args(&["run", "cancel", &r.id.to_string(), "-R", repo]),
        ),
        _ => return None,
    };
    Some(Cmd {
        label: format!("{verb} {}", item.id()),
        args: list,
        stdin: None,
    })
}

/// Turn a filled-in form into a command, or explain what is missing.
pub fn submit(form: &Form) -> std::result::Result<Cmd, String> {
    let repo = format!("--repo={}", form.repo);
    let text = |i: usize| form.text(i);
    let need = |i: usize| {
        let value = form.text(i).trim();
        if value.is_empty() {
            Err(format!("{} is required", form.fields[i].label))
        } else {
            Ok(value.to_string())
        }
    };
    let number = match &form.item {
        Some(Item::Ticket(t)) => t.number.to_string(),
        _ => String::new(),
    };
    let mut stdin = None;
    let list = match (form.act, &form.item) {
        (Act::Comment, Some(Item::Ticket(t))) => {
            need(0)?;
            stdin = Some(text(0).to_string());
            let kind = if t.head.is_empty() { "issue" } else { "pr" };
            args(&[kind, "comment", &number, &repo, "--body-file=-"])
        }
        (Act::Approve, Some(Item::Ticket(_))) => {
            let mut a = args(&["pr", "review", &number, &repo, "--approve"]);
            if !text(0).trim().is_empty() {
                a.push("--body-file=-".into());
                stdin = Some(text(0).to_string());
            }
            a
        }
        (Act::RequestChanges, Some(Item::Ticket(_))) => {
            need(0)?;
            stdin = Some(text(0).to_string());
            args(&[
                "pr",
                "review",
                &number,
                &repo,
                "--request-changes",
                "--body-file=-",
            ])
        }
        (Act::Merge, Some(Item::Ticket(t))) => {
            let method = ["--merge", "--squash", "--rebase"][form.choice(0)];
            // Refuse to merge commits pushed after the list was loaded.
            let head = format!("--match-head-commit={}", t.oid);
            let mut a = args(&["pr", "merge", &number, &repo, method, &head]);
            if form.toggle(1) {
                a.push("--delete-branch".into());
            }
            a
        }
        (Act::Labels, Some(Item::Ticket(t))) => edit_list(form, &t.labels, "label", &number)?,
        (Act::Assignees, Some(Item::Ticket(t))) => {
            edit_list(form, &t.assignees, "assignee", &number)?
        }
        (Act::New, None) if form.tab == Tab::Issues => {
            let mut a = args(&["issue", "create", &repo, "--body-file=-"]);
            a.push(format!("--title={}", need(0)?));
            for label in lines(text(2)) {
                a.push(format!("--label={}", csv(label)));
            }
            stdin = Some(text(1).to_string());
            a
        }
        (Act::New, None) => {
            let tag = need(0)?;
            let mut a = args(&["release", "create", &repo]);
            a.push(format!("--title={}", text(1).trim()));
            if !text(2).trim().is_empty() {
                a.push(format!("--target={}", text(2).trim()));
            }
            if form.toggle(4) {
                a.push("--generate-notes".into());
            }
            if !form.toggle(4) || !text(3).trim().is_empty() {
                a.push("--notes-file=-".into());
                stdin = Some(text(3).to_string());
            }
            a.push(format!("--draft={}", form.toggle(5)));
            a.push(format!("--prerelease={}", form.toggle(6)));
            a.extend(["--".into(), tag]);
            a
        }
        (Act::Edit, Some(Item::Release(r))) => {
            stdin = Some(text(1).to_string());
            args(&[
                "release",
                "edit",
                &repo,
                &format!("--title={}", text(0).trim()),
                "--notes-file=-",
                &format!("--draft={}", form.toggle(2)),
                &format!("--prerelease={}", form.toggle(3)),
                "--",
                &r.tag,
            ])
        }
        (Act::Download, Some(Item::Release(r))) => args(&[
            "release",
            "download",
            &repo,
            &format!("--pattern={}", need(0)?),
            &format!("--dir={}", need(1)?),
            "--",
            &r.tag,
        ]),
        (Act::Trigger, None) => {
            let mut a = args(&["workflow", "run", &form.extra, &repo]);
            if !text(0).trim().is_empty() {
                a.push(format!("--ref={}", text(0).trim()));
            }
            for line in lines(text(1)) {
                if !line.contains('=') {
                    return Err(format!("input {line:?} must look like key=value"));
                }
                a.push(format!("--raw-field={line}"));
            }
            a
        }
        _ => return Err("this action does not apply here".into()),
    };
    Ok(Cmd {
        label: form.title.clone(),
        args: list,
        stdin,
    })
}

fn edit_list(
    form: &Form,
    old: &[String],
    what: &str,
    number: &str,
) -> std::result::Result<Vec<String>, String> {
    let new = lines(form.text(0));
    let mut a = args(&["issue", "edit", number, &format!("--repo={}", form.repo)]);
    let fixed = a.len();
    for name in new.iter().filter(|n| !old.iter().any(|o| o == *n)) {
        a.push(format!("--add-{what}={}", csv(name)));
    }
    for name in old.iter().filter(|o| !new.contains(&o.as_str())) {
        a.push(format!("--remove-{what}={}", csv(name)));
    }
    if a.len() == fixed {
        return Err("nothing changed".into());
    }
    Ok(a)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::form::Kind;

    fn ticket(head: &str) -> Item {
        Item::Ticket(Ticket {
            repo: "o/r".into(),
            number: 7,
            state: "OPEN".into(),
            head: head.into(),
            oid: "abc".into(),
            labels: vec!["bug".into(), "a,b".into()],
            ..Ticket::default()
        })
    }

    fn set(form: &mut Form, i: usize, value: &str) {
        form.fields[i].kind = Kind::text(value, true);
    }

    #[test]
    fn validates_repo_names() {
        for ok in ["a/b", "rust-lang/rust", "a/.github"] {
            assert!(valid_repo(ok), "{ok}");
        }
        for bad in ["", "a", "a/b/c", "-a/b", "a/..", "a/b c", "a/b?x", "/b"] {
            assert!(!valid_repo(bad), "{bad}");
        }
    }

    #[test]
    fn builds_search_queries() {
        let mut q = Query {
            tab: Tab::Prs,
            scope: None,
            filter: 0,
            state: 0,
            cursor: None,
        };
        let tail = "archived:false sort:updated-desc";
        assert_eq!(
            search_query(&q, "me"),
            format!("is:pr user:me is:open {tail}")
        );
        q.filter = 1;
        assert_eq!(
            search_query(&q, "me"),
            format!("is:pr involves:@me is:open {tail}")
        );
        q.scope = Some("o/r".into());
        q.filter = 0;
        q.state = 3;
        assert_eq!(search_query(&q, "me"), format!("is:pr repo:o/r {tail}"));
    }

    #[test]
    fn merge_pins_the_reviewed_commit() {
        let form = Form::new(Tab::Prs, Act::Merge, "o/r", Some(ticket("x")), "").unwrap();
        let cmd = submit(&form).unwrap();
        assert!(cmd.args.contains(&"--match-head-commit=abc".to_string()));
        assert!(cmd.args.contains(&"--merge".to_string()));
        assert!(!cmd.args.contains(&"--delete-branch".to_string()));
    }

    #[test]
    fn comment_uses_stdin_and_rejects_blank() {
        let mut form = Form::new(Tab::Issues, Act::Comment, "o/r", Some(ticket("")), "").unwrap();
        assert!(submit(&form).is_err());
        set(&mut form, 0, "--danger");
        let cmd = submit(&form).unwrap();
        assert_eq!(cmd.stdin.as_deref(), Some("--danger"));
        assert_eq!(cmd.args[0], "issue");
        assert!(!cmd.args.iter().any(|a| a.contains("danger")));
    }

    #[test]
    fn labels_are_diffed_and_quoted() {
        let mut form = Form::new(Tab::Issues, Act::Labels, "o/r", Some(ticket("")), "").unwrap();
        assert_eq!(submit(&form), Err("nothing changed".into()));
        set(&mut form, 0, "bug\nnew");
        let cmd = submit(&form).unwrap();
        assert_eq!(cmd.args[4..], ["--add-label=new", "--remove-label=\"a,b\""]);
    }

    #[test]
    fn release_tag_cannot_become_a_flag() {
        let mut form = Form::new(Tab::Releases, Act::New, "o/r", None, "").unwrap();
        assert!(submit(&form).is_err());
        set(&mut form, 0, "--draft");
        let cmd = submit(&form).unwrap();
        assert_eq!(cmd.args[cmd.args.len() - 2..], ["--", "--draft"]);
    }

    #[test]
    fn workflow_inputs_need_key_value() {
        let mut form = Form::new(Tab::Runs, Act::Trigger, "o/r", None, "42").unwrap();
        set(&mut form, 1, "oops");
        assert!(submit(&form).is_err());
        set(&mut form, 1, "env=prod");
        assert!(
            submit(&form)
                .unwrap()
                .args
                .contains(&"--raw-field=env=prod".into())
        );
    }

    #[test]
    fn toggle_follows_state() {
        let Item::Ticket(mut t) = ticket("x") else {
            unreachable!()
        };
        assert_eq!(
            simple(Act::Toggle, &Item::Ticket(t.clone())).unwrap().args[..2],
            ["pr", "close"]
        );
        t.state = "MERGED".into();
        assert_eq!(simple(Act::Toggle, &Item::Ticket(t)), None);
    }
}
