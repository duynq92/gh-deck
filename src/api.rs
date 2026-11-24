//! GitHub data: list queries, detail documents and mutating commands.
//! `--jq` trims responses inside `gh`, so only displayed fields reach us.

use crate::doc::Source;
use crate::form::Form;
use crate::gh::{self, Cmd, GhError, Guard, Keep, Result};
use crate::keys::{Act, Tab};
use crate::review::{self, split};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use std::thread;

/// Rows a list shows at first.
const PAGE: usize = 50;
const RUN_REPOS: usize = 12;
const PARALLEL: usize = 4;
const ALL_REPOS: &str = "OWNER,COLLABORATOR,ORGANIZATION_MEMBER";
/// Repositories to list at most.
pub const MAX_REPOS: usize = 500;
/// Filter value: review requested from any team the user belongs to.
pub const MY_TEAMS: &str = "my-teams";

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Ticket {
    /// GraphQL node id, used to load reviews.
    #[serde(default)]
    pub id: String,
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
    /// Loaded after the list, because GitHub is slow to compute it.
    #[serde(skip)]
    pub reviews: Option<Reviews>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Reviews {
    pub id: String,
    pub decision: Option<String>,
    pub checks: Option<String>,
    pub reviewers: Vec<Reviewer>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Reviewer {
    pub name: String,
    /// `REQUESTED` or the state of the latest review.
    pub state: String,
}

impl Reviewer {
    pub fn mark(&self) -> char {
        match self.state.as_str() {
            "APPROVED" => '✓',
            "CHANGES_REQUESTED" => '✗',
            "REQUESTED" => '●',
            "DISMISSED" => '-',
            _ => '·',
        }
    }
}

impl Reviews {
    pub fn approval(&self) -> &'static str {
        let any = |state| self.reviewers.iter().any(|r| r.state == state);
        match self.decision.as_deref() {
            Some("APPROVED") => "✓ approved",
            Some("CHANGES_REQUESTED") => "✗ changes",
            Some(_) => "● required",
            // Without branch protection GitHub decides nothing.
            None if any("CHANGES_REQUESTED") => "✗ changes",
            None if any("APPROVED") => "✓ approved",
            None if any("REQUESTED") => "● pending",
            None => "· none",
        }
    }
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
        let reviews = match self {
            Item::Ticket(t) => t.reviews.as_ref(),
            _ => None,
        };
        let reviewer = reviews
            .iter()
            .flat_map(|r| &r.reviewers)
            .any(|r| r.name.to_lowercase().contains(needle));
        reviewer
            || format!(
                "{} {} {} {}",
                self.id(),
                self.title(),
                self.state(),
                author.unwrap_or("")
            )
            .to_lowercase()
            .contains(needle)
    }

    /// Preview as a document.
    pub fn source(&self) -> Source {
        match self {
            Item::Run(_) => Source::plain(self.preview()),
            _ => Source::preview(&self.preview()),
        }
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
                if let Some(r) = &t.reviews {
                    s += &format!(
                        "Approval: {}
",
                        r.approval()
                    );
                    if let Some(c) = &r.checks {
                        s += &format!(
                            "Checks: {}
",
                            c.to_lowercase()
                        );
                    }
                    let names: Vec<String> = r
                        .reviewers
                        .iter()
                        .map(|r| format!("{} {}", r.mark(), r.name))
                        .collect();
                    if !names.is_empty() {
                        s += &format!(
                            "Reviewers: {}
",
                            names.join(", ")
                        );
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
    pub orgs: Vec<String>,
    /// `org/team` of every team the user is in; loaded separately.
    #[serde(default)]
    pub teams: Vec<String>,
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

/// Label and value of something the user can choose.
pub type Choice = (String, String);

/// What a list should show. Choices in one field are alternatives; an empty
/// field does not restrict anything.
#[derive(Debug, Clone)]
pub struct Query {
    pub tab: Tab,
    /// Organizations, or the user.
    pub orgs: Vec<Choice>,
    pub repos: Vec<Choice>,
    /// Search qualifiers or `MY_TEAMS`.
    pub filters: Vec<Choice>,
    pub states: Vec<Choice>,
    pub cursor: Option<String>,
}

fn choice(name: &str, value: &str) -> Choice {
    (name.to_string(), value.to_string())
}

/// Filter names and values. Empty when the tab has no filters.
pub fn filters(tab: Tab, teams: &[String]) -> Vec<Choice> {
    let item = choice;
    match tab {
        Tab::Prs => {
            let mut all = vec![
                item("Opened by me", "author:@me"),
                item("Review requested: me", "user-review-requested:@me"),
            ];
            all.push(item("Involves me", "involves:@me"));
            all.push(item("Review requested: my teams", MY_TEAMS));
            for team in teams {
                let value = format!("team-review-requested:{team}");
                all.push((format!("Review requested: {team}"), value));
            }
            all
        }
        Tab::Issues => vec![
            item("Opened by me", "author:@me"),
            item("Assigned to me", "assignee:@me"),
            item("Involves me", "involves:@me"),
        ],
        Tab::Releases | Tab::Runs => Vec::new(),
    }
}

/// Pull requests start with the ones that concern the user.
pub fn default_filters(tab: Tab) -> Vec<Choice> {
    match tab {
        Tab::Prs => filters(tab, &[]),
        Tab::Issues | Tab::Releases | Tab::Runs => Vec::new(),
    }
}

/// State names and values. Empty when the tab has no states.
pub fn states(tab: Tab) -> Vec<Choice> {
    match tab {
        Tab::Prs => vec![
            choice("Open", "is:open"),
            choice("Merged", "is:merged"),
            choice("Closed", "(is:closed is:unmerged)"),
        ],
        Tab::Issues => vec![choice("Open", "is:open"), choice("Closed", "is:closed")],
        Tab::Releases => Vec::new(),
        Tab::Runs => vec![
            choice("Failed", "failure"),
            choice("Running", "in_progress"),
            choice("Queued", "queued"),
            choice("Succeeded", "success"),
            choice("Cancelled", "cancelled"),
        ],
    }
}

/// Pull requests and issues start with the open ones.
pub fn default_states(tab: Tab) -> Vec<Choice> {
    match tab {
        Tab::Prs | Tab::Issues => states(tab).into_iter().take(1).collect(),
        Tab::Releases | Tab::Runs => Vec::new(),
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

/// One page of repositories and the cursor of the next page.
pub fn repos(after: Option<&str>) -> Result<(Repos, Option<String>)> {
    #[derive(Deserialize)]
    struct Out {
        #[serde(flatten)]
        repos: Repos,
        cursor: Option<String>,
    }
    let query = format!(
        "query=query($after:String){{viewer{{login organizations(first:100){{nodes{{login}}}} \
         repositories(first:100,after:$after,affiliations:[{ALL_REPOS}],\
         ownerAffiliations:[{ALL_REPOS}],orderBy:{{field:PUSHED_AT,direction:DESC}}){{\
         pageInfo{{hasNextPage endCursor}} nodes{{nameWithOwner isArchived}}}}}}}}"
    );
    let jq = ".data.viewer|{login,orgs:[.organizations.nodes[].login],\
         names:[.repositories.nodes[]|select(.isArchived|not)|.nameWithOwner],\
         cursor:(.repositories.pageInfo|if .hasNextPage then .endCursor else null end)}";
    let after = after.map(|c| format!("after={c}"));
    let mut list = vec!["api", "graphql", "-f", &query, "--jq", jq];
    if let Some(after) = &after {
        list.extend(["-f", after]);
    }
    let out: Out = fetch(&list)?;
    Ok((out.repos, out.cursor))
}

/// Every team the user belongs to.
pub fn teams(login: &str) -> Result<Vec<String>> {
    fetch(&[
        "api",
        "graphql",
        "-f",
        "query=query($login:String!){viewer{organizations(first:100){nodes{\
         teams(first:100,userLogins:[$login]){nodes{combinedSlug}}}}}}",
        "-f",
        &format!("login={login}"),
        "--jq",
        "[.data.viewer.organizations.nodes[].teams.nodes[].combinedSlug]",
    ])
}

/// Review state of the given pull requests.
pub fn reviews(ids: &[String]) -> Result<Vec<Reviews>> {
    let ids: Vec<String> = ids.iter().map(|id| format!("ids[]={id}")).collect();
    let mut list = vec!["api", "graphql"];
    for id in &ids {
        list.extend(["-f", id]);
    }
    list.extend([
        "-f",
        "query=query($ids:[ID!]!){nodes(ids:$ids){...on PullRequest{id author{login} \
         reviewDecision reviewRequests(first:20){nodes{requestedReviewer{\
         ...on User{login} ...on Bot{login} ...on Mannequin{login} ...on Team{slug}}}} \
         latestReviews(first:20){nodes{state author{login}}} \
         commits(last:1){nodes{commit{statusCheckRollup{state}}}}}}}",
        "--jq",
        "[.data.nodes[]|select(.id)|.author.login as $me\
         |([.reviewRequests.nodes[].requestedReviewer|select(.)\
         |{name:(.login//(\"@\"+.slug)),state:\"REQUESTED\"}]) as $asked\
         |{id,decision:.reviewDecision,checks:.commits.nodes[0].commit.statusCheckRollup.state,\
         reviewers:([.latestReviews.nodes[]|select(.author and .author.login!=$me)\
         |{name:.author.login,state}\
         |select(.name as $n|$asked|any(.name==$n)|not)]+$asked)}]",
    ]);
    fetch(&list)
}

pub fn list(q: &Query, repos: &Repos) -> Result<Page> {
    match q.tab {
        Tab::Prs | Tab::Issues => tickets(q, repos),
        Tab::Releases => releases(q),
        Tab::Runs => runs(q, repos),
    }
}

/// `a`, `(a OR b)` or nothing.
fn any_of(parts: Vec<String>) -> String {
    match parts.as_slice() {
        [] => String::new(),
        [one] => one.clone(),
        many => format!("({})", many.join(" OR ")),
    }
}

/// Query for GitHub's advanced issue search, the one that understands OR.
fn search_query(q: &Query, repos: &Repos) -> String {
    let kind = if q.tab == Tab::Prs {
        "is:pr"
    } else {
        "is:issue"
    };
    let place = if !q.repos.is_empty() {
        q.repos.iter().map(|r| format!("repo:{}", r.1)).collect()
    } else if !q.orgs.is_empty() {
        q.orgs.iter().map(|o| format!("user:{}", o.1)).collect()
    } else if q.filters.is_empty() {
        [&repos.login]
            .into_iter()
            .chain(&repos.orgs)
            .map(|o| format!("user:{o}"))
            .collect()
    } else {
        // "Mine" filters are not limited to known repositories.
        Vec::new()
    };
    let mut filters = Vec::new();
    for (_, value) in &q.filters {
        if value == MY_TEAMS {
            let teams = repos.teams.iter();
            filters.extend(teams.map(|t| format!("team-review-requested:{t}")));
        } else {
            filters.push(value.clone());
        }
    }
    filters.dedup();
    let parts = [
        kind.to_string(),
        any_of(place),
        any_of(filters),
        any_of(q.states.iter().map(|s| s.1.clone()).collect()),
        "archived:false sort:updated-desc".to_string(),
    ];
    let parts: Vec<String> = parts.into_iter().filter(|s| !s.is_empty()).collect();
    parts.join(" ")
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
        format!("... on PullRequest{{{TICKET} id isDraft headRefName headRefOid baseRefName}}")
    } else {
        format!("... on Issue{{{TICKET}}}")
    };
    if q.filters.iter().all(|f| f.1 == MY_TEAMS) && !q.filters.is_empty() && repos.teams.is_empty()
    {
        // Without a team the query would match everything.
        return Ok(Page {
            warning: Some("you are in no team, or all your teams are excluded".into()),
            ..Page::default()
        });
    }
    let search = search_query(q, repos);
    let query = format!(
        "query=query($q:String!,$after:String){{search(type:ISSUE_ADVANCED,first:{PAGE},query:$q,\
         after:$after){{issueCount pageInfo{{hasNextPage endCursor}} nodes{{{kind}}}}}}}"
    );
    let search = format!("q={search}");
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
         oid:(.headRefOid//\"\"),base:(.baseRefName//\"\"),id:(.id//\"\")}]}",
    ]);
    let out: Out = fetch(&list)?;
    // The search index lags behind changes; trust the live state instead.
    let wanted: Vec<String> = q.states.iter().map(|s| s.0.to_uppercase()).collect();
    let items = out
        .items
        .into_iter()
        .filter(|t| wanted.is_empty() || wanted.contains(&t.state))
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
    // One aliased field per repository or owner, all in a single request.
    let order = "orderBy:{field:PUSHED_AT,direction:DESC}";
    let (query, nodes) = if !q.repos.is_empty() {
        let count = share(q.repos.len());
        let mut query = String::new();
        for (n, (_, repo)) in q.repos.iter().enumerate() {
            let (owner, name) = split(repo)?;
            query += &format!(
                "r{n}:repository(owner:\"{owner}\",name:\"{name}\"){{{}}} ",
                fields(count)
            );
        }
        (query, "[.data[]|select(.)]")
    } else if !q.orgs.is_empty() {
        let mut query = String::new();
        for (n, (_, org)) in q.orgs.iter().enumerate() {
            query += &format!(
                "o{n}:repositoryOwner(login:\"{org}\"){{repositories(first:50,\
                 ownerAffiliations:[OWNER],{order}){{nodes{{{}}}}}}} ",
                fields(3)
            );
        }
        (query, "[.data[]|select(.)|.repositories.nodes[]]")
    } else {
        let query = format!(
            "viewer{{repositories(first:50,affiliations:[{ALL_REPOS}],\
             ownerAffiliations:[{ALL_REPOS}],{order}){{nodes{{{}}}}}}}",
            fields(3)
        );
        (query, "[.data[].repositories.nodes[]]")
    };
    let list: Vec<Release> = fetch(&[
        "api",
        "graphql",
        "-f",
        &format!("query={{{query}}}"),
        "--jq",
        &format!("{nodes}|{map}"),
    ])?;
    let total = list.len();
    let list: Vec<Release> = list.into_iter().take(PAGE).collect();
    Ok(Page {
        items: list.into_iter().map(Item::Release).collect(),
        total,
        ..Page::default()
    })
}

/// Rows to ask of each of `n` repositories, enough to fill a page together.
fn share(n: usize) -> usize {
    (2 * PAGE / n.max(1)).clamp(10, PAGE)
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
    let names: Vec<&str> = if q.repos.is_empty() {
        // REST has no cross-repository endpoint: one request per recent repository.
        let owner = |n: &str| n.split('/').next().unwrap_or("").to_string();
        repos
            .names
            .iter()
            .filter(|n| q.orgs.is_empty() || q.orgs.iter().any(|o| o.1 == owner(n)))
            .take(RUN_REPOS)
            .map(String::as_str)
            .collect()
    } else {
        q.repos.iter().map(|r| r.1.as_str()).collect()
    };
    let count = share(names.len());
    // The endpoint takes a single status.
    let statuses: Vec<&str> = match q.states.as_slice() {
        [] => vec![""],
        many => many.iter().map(|s| s.1.as_str()).collect(),
    };
    let jobs: Vec<(&str, &str)> = names
        .iter()
        .flat_map(|n| statuses.iter().map(move |s| (*n, *s)))
        .collect();
    let mut all = Vec::new();
    let mut errors = Vec::new();
    for batch in jobs.chunks(PARALLEL) {
        let results: Vec<_> = thread::scope(|s| {
            let handles: Vec<_> = batch
                .iter()
                .map(|(repo, status)| s.spawn(move || repo_runs(repo, status, count)))
                .collect();
            handles
                .into_iter()
                .map(|h| {
                    h.join()
                        .unwrap_or_else(|_| Err(GhError("worker panicked".into())))
                })
                .collect()
        });
        for ((repo, _), result) in batch.iter().zip(results) {
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
        items: all.into_iter().take(PAGE).map(Item::Run).collect(),
        cursor: None,
        warning: (!errors.is_empty())
            .then(|| format!("{} requests failed: {}", errors.len(), errors.join("; "))),
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

/// Load a detail document.
pub fn detail(act: Act, item: &Item) -> Result<Source> {
    match (act, item) {
        (Act::Diff | Act::Info, Item::Ticket(t)) if !t.head.is_empty() => pull(act, item, t),
        // Job lists and logs are not Markdown.
        (Act::Info, Item::Ticket(_) | Item::Release(_)) => Ok(Source::preview(&text(act, item)?)),
        _ => Ok(Source::plain(text(act, item)?)),
    }
}

/// A pull request: its changes and discussions, and for Info its description
/// and conversation.
fn pull(act: Act, item: &Item, t: &Ticket) -> Result<Source> {
    let (diff, talk, comments) = thread::scope(|s| {
        let diff = s.spawn(|| text(Act::Diff, item));
        let talk = s.spawn(|| review::load(&t.repo, t.number));
        let comments = (act == Act::Info).then(|| text(act, item));
        let lost = || Err(GhError("worker panicked".into()));
        (
            diff.join().unwrap_or_else(|_| lost()),
            talk.join()
                .unwrap_or_else(|_| Err(GhError("worker panicked".into()))),
            comments,
        )
    });
    let mut source = Source {
        markdown: true,
        ..Source::default()
    };
    // What failed is said; the rest is still useful.
    let mut missing = String::new();
    let talk = match talk {
        Ok(talk) => talk,
        Err(e) => {
            missing += &format!("── Discussions not available: {e}\n\n");
            review::Talk::default()
        }
    };
    match comments {
        Some(comments) => {
            source.tail = format!("── Conversation\n{}", comments?);
            let preview = item.preview();
            let (head, body) = preview.split_once("\n\n").unwrap_or((&preview, ""));
            source.head = format!("{head}\n{}", talk.summary());
            match diff {
                Ok(diff) => source.diff = Some(diff),
                // GitHub refuses very large diffs.
                Err(e) => missing += &format!("── Changes not available: {e}\n\n"),
            }
            source.body = format!("{missing}── Description\n{body}");
        }
        None => {
            source.diff = Some(diff?);
            source.body = missing;
        }
    }
    source.threads = talk.threads;
    source.oid = talk.oid;
    Ok(source)
}

fn text(act: Act, item: &Item) -> Result<String> {
    let repo = item.repo();
    let get = |list: &[&str], keep: Keep| gh::run(&args(list), None, keep);
    match (act, item) {
        (Act::Info, Item::Ticket(t)) => {
            let n = t.number.to_string();
            let (kind, fields) = if t.head.is_empty() {
                ("issue", "comments")
            } else {
                ("pr", "comments,reviews,reviewDecision")
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
                    "(if (.reviewDecision//\"\")!=\"\" then \"Review: \\(.reviewDecision|ascii_downcase|gsub(\"_\";\" \"))\\n\" else empty end),([.comments[]|{a:.author.login,t:.createdAt,b:.body,k:\"commented\"}]\
                     +[.reviews[]?|select(.submittedAt)|{a:.author.login,t:.submittedAt,b:.body,\
                     k:(.state|ascii_downcase)}]|sort_by(.t)[]\
                     |\"── @\\(.a//\"ghost\") \\(.k) · \\(.t[:10])\\n\\(.b)\\n\")",
                ],
                gh::JSON,
            )?;
            let thread = if thread.trim().is_empty() {
                "── No comments yet"
            } else {
                &thread
            };
            Ok(if t.head.is_empty() {
                format!("{}\n\n{thread}", item.preview())
            } else {
                thread.to_string()
            })
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

/// Current text of a pull request.
#[derive(Debug, Default, Deserialize)]
pub struct PrText {
    pub mine: bool,
    pub title: String,
    pub body: String,
}

fn text_query(repo: &str, number: u64, jq: &str) -> Result<Vec<String>> {
    let (owner, name) = split(repo)?;
    Ok(args(&[
        "api",
        "graphql",
        "-f",
        "query=query($o:String!,$n:String!,$number:Int!){repository(owner:$o,name:$n){\
         pullRequest(number:$number){viewerDidAuthor title body}}}",
        "-f",
        &format!("o={owner}"),
        "-f",
        &format!("n={name}"),
        "-F",
        &format!("number={number}"),
        "--jq",
        &format!(".data.repository.pullRequest|{jq}"),
    ]))
}

pub fn pr_text(repo: &str, number: u64) -> Result<PrText> {
    let list = text_query(repo, number, "{mine:.viewerDidAuthor,title,body}")?;
    let text = gh::run(&list, None, gh::JSON)?;
    serde_json::from_str(&text).map_err(|e| GhError(format!("unexpected gh response: {e}")))
}

/// `field` is `title` or `body`. `tojson` keeps every byte of it.
fn text_guard(repo: &str, number: u64, field: &str, expected: &str) -> Guard {
    Guard {
        // An invalid name cannot come from GitHub; an empty query would fail safely.
        args: text_query(repo, number, &format!(".{field}|tojson")).unwrap_or_default(),
        expected: expected.to_string(),
    }
}

/// Replace a description that is still `old`.
pub fn edit_body(repo: &str, number: u64, old: &str, new: String) -> Cmd {
    Cmd {
        label: format!("Edit description of {repo}#{number}"),
        args: args(&[
            "pr",
            "edit",
            &number.to_string(),
            &format!("--repo={repo}"),
            "--body-file=-",
        ]),
        stdin: Some(new),
        guard: Some(text_guard(repo, number, "body", old)),
        ..Cmd::default()
    }
}

/// What to know before merging.
#[derive(Debug, Default)]
pub struct Readiness {
    pub oid: String,
    pub conflicts: bool,
    /// Allowed methods, the preferred one first.
    pub methods: Vec<&'static str>,
    /// Good and bad news.
    pub notes: Vec<(bool, String)>,
}

#[derive(Debug, Default, Deserialize)]
struct MergeFacts {
    oid: String,
    mergeable: String,
    state: String,
    decision: Option<String>,
    /// Name and state of every check.
    checks: Vec<(String, String)>,
    unresolved: usize,
    merge: bool,
    squash: bool,
    rebase: bool,
    preferred: String,
    deletes: bool,
}

pub fn readiness(repo: &str, number: u64) -> Result<Readiness> {
    let (owner, name) = split(repo)?;
    let facts: MergeFacts = fetch(&[
        "api",
        "graphql",
        "-f",
        "query=query($o:String!,$n:String!,$number:Int!){repository(owner:$o,name:$n){\
         mergeCommitAllowed squashMergeAllowed rebaseMergeAllowed viewerDefaultMergeMethod \
         deleteBranchOnMerge pullRequest(number:$number){headRefOid mergeable mergeStateStatus \
         reviewDecision reviewThreads(first:100){nodes{isResolved}} \
         commits(last:1){nodes{commit{statusCheckRollup{contexts(first:100){nodes{\
         ...on CheckRun{name conclusion status} ...on StatusContext{context state}}}}}}}}}}",
        "-f",
        &format!("o={owner}"),
        "-f",
        &format!("n={name}"),
        "-F",
        &format!("number={number}"),
        "--jq",
        ".data.repository|.pullRequest as $p|{oid:$p.headRefOid,mergeable:$p.mergeable,\
         state:$p.mergeStateStatus,decision:$p.reviewDecision,\
         checks:[$p.commits.nodes[0].commit.statusCheckRollup.contexts.nodes[]?\
         |[(.name//.context//\"\"),(.conclusion//.state//.status//\"\")]],\
         unresolved:([$p.reviewThreads.nodes[]|select(.isResolved|not)]|length),\
         merge:.mergeCommitAllowed,squash:.squashMergeAllowed,rebase:.rebaseMergeAllowed,\
         preferred:.viewerDefaultMergeMethod,deletes:.deleteBranchOnMerge}",
    ])?;
    Ok(judge(facts))
}

fn judge(f: MergeFacts) -> Readiness {
    let mut methods = Vec::new();
    for (allowed, id, name) in [
        (f.merge, "MERGE", "Merge commit"),
        (f.squash, "SQUASH", "Squash"),
        (f.rebase, "REBASE", "Rebase"),
    ] {
        if allowed {
            let at = if f.preferred == id { 0 } else { methods.len() };
            methods.insert(at, name);
        }
    }
    let mut notes = Vec::new();
    let mut note = |good: bool, text: String| notes.push((good, text));
    match f.mergeable.as_str() {
        "MERGEABLE" => note(true, "no conflicts".into()),
        "CONFLICTING" => note(false, "conflicts with the base branch".into()),
        _ => note(false, "GitHub is still checking for conflicts".into()),
    }
    match f.decision.as_deref() {
        Some("APPROVED") => note(true, "approved".into()),
        Some("CHANGES_REQUESTED") => note(false, "changes requested".into()),
        Some(_) => note(false, "approval is still required".into()),
        None => {}
    }
    let named = |wanted: &[&str]| -> Vec<&str> {
        let checks = f.checks.iter().filter(|c| wanted.contains(&c.1.as_str()));
        checks.map(|c| c.0.as_str()).collect()
    };
    let failed = named(&[
        "FAILURE",
        "ERROR",
        "TIMED_OUT",
        "CANCELLED",
        "ACTION_REQUIRED",
        "STARTUP_FAILURE",
        "STALE",
    ]);
    let running = named(&[
        "QUEUED",
        "IN_PROGRESS",
        "PENDING",
        "WAITING",
        "REQUESTED",
        "EXPECTED",
    ]);
    let list = |names: &[&str]| {
        let more = names.len().saturating_sub(3);
        let first = names.iter().take(3).copied().collect::<Vec<_>>().join(", ");
        if more > 0 {
            format!("{first} +{more}")
        } else {
            first
        }
    };
    if !failed.is_empty() {
        note(false, format!("checks failed: {}", list(&failed)));
    }
    if !running.is_empty() {
        note(false, format!("checks running: {}", list(&running)));
    }
    if failed.is_empty() && running.is_empty() && !f.checks.is_empty() {
        note(true, format!("all {} checks passed", f.checks.len()));
    }
    match f.unresolved {
        0 => {}
        n => note(false, format!("{n} unresolved discussions")),
    }
    match f.state.as_str() {
        "BLOCKED" => note(false, "blocked by the branch rules".into()),
        "BEHIND" => note(false, "behind the base branch".into()),
        _ => {}
    }
    if f.deletes {
        note(true, "the branch is deleted automatically".into());
    }
    Readiness {
        oid: f.oid,
        conflicts: f.mergeable == "CONFLICTING",
        methods,
        notes,
    }
}

/// Who can be asked to review a pull request.
#[derive(Debug, Default)]
pub struct Candidates {
    pub mine: bool,
    /// Logins and `org/team`s asked so far.
    pub requested: Vec<String>,
    pub people: Vec<Choice>,
}

pub fn candidates(repo: &str, number: u64) -> Result<Candidates> {
    #[derive(Deserialize)]
    struct Out {
        mine: bool,
        author: Option<String>,
        requested: Vec<String>,
        cursor: Option<String>,
        people: Vec<(String, Option<String>)>,
    }
    let (owner, name) = split(repo)?;
    let mut all = Candidates::default();
    let mut after: Option<String> = None;
    for _ in 0..5 {
        let (owner, name) = (format!("o={owner}"), format!("n={name}"));
        let number = format!("number={number}");
        let mut list = vec![
            "api",
            "graphql",
            "-f",
            "query=query($o:String!,$n:String!,$number:Int!,$after:String){\
             repository(owner:$o,name:$n){pullRequest(number:$number){viewerDidAuthor \
             author{login} reviewRequests(first:100){nodes{requestedReviewer{\
             ...on User{login} ...on Bot{login} ...on Mannequin{login} \
             ...on Team{combinedSlug}}}}} \
             assignableUsers(first:100,after:$after){pageInfo{hasNextPage endCursor} \
             nodes{login name}}}}",
            "-f",
            &owner,
            "-f",
            &name,
            "-F",
            &number,
            "--jq",
            ".data.repository|{mine:.pullRequest.viewerDidAuthor,\
             author:.pullRequest.author.login,\
             requested:[.pullRequest.reviewRequests.nodes[].requestedReviewer|select(.)\
             |(.login//.combinedSlug)],\
             cursor:(.assignableUsers.pageInfo|if .hasNextPage then .endCursor else null end),\
             people:[.assignableUsers.nodes[]|[.login,.name]]}",
        ];
        if let Some(after) = &after {
            list.extend(["-f", after]);
        }
        let out: Out = fetch(&list)?;
        all.mine = out.mine;
        all.requested = out.requested;
        // Nobody can review their own pull request.
        let others = out
            .people
            .into_iter()
            .filter(|p| Some(&p.0) != out.author.as_ref());
        all.people.extend(others.map(|(login, name)| {
            let label = match name {
                Some(name) if !name.is_empty() => format!("{login}  ({name})"),
                _ => login.clone(),
            };
            (label, login)
        }));
        match out.cursor {
            Some(cursor) => after = Some(format!("after={cursor}")),
            None => break,
        }
    }
    let teams = repo_teams(repo, owner)?;
    all.people
        .extend(teams.into_iter().map(|t| (format!("{t}  (team)"), t)));
    Ok(all)
}

/// Teams with access, or every team of the organization when GitHub does not
/// let the user see who has access.
fn repo_teams(repo: &str, owner: &str) -> Result<Vec<String>> {
    let exact: Result<Vec<String>> = fetch(&[
        "api",
        &format!("repos/{repo}/teams?per_page=100"),
        "--jq",
        "[.[].slug]",
    ]);
    if let Ok(slugs) = exact {
        return Ok(slugs.iter().map(|s| format!("{owner}/{s}")).collect());
    }
    fetch(&[
        "api",
        "graphql",
        "-f",
        "query=query($o:String!){repositoryOwner(login:$o){\
         ...on Organization{teams(first:100){nodes{combinedSlug}}}}}",
        "-f",
        &format!("o={owner}"),
        "--jq",
        "[.data.repositoryOwner.teams.nodes[]?.combinedSlug]",
    ])
}

/// Ask for and withdraw reviews; `None` when nothing changes.
pub fn request_reviews(repo: &str, number: u64, old: &[String], new: &[String]) -> Option<Cmd> {
    let missing = |a: &[String], b: &[String]| -> Vec<String> {
        a.iter().filter(|x| !b.contains(x)).cloned().collect()
    };
    let (add, remove) = (missing(new, old), missing(old, new));
    let mut list = args(&["pr", "edit", &number.to_string(), &format!("--repo={repo}")]);
    let mut what = Vec::new();
    if !add.is_empty() {
        list.push(format!("--add-reviewer={}", add.join(",")));
        what.push(format!("request review from {}", add.join(", ")));
    }
    if !remove.is_empty() {
        list.push(format!("--remove-reviewer={}", remove.join(",")));
        what.push(format!("withdraw request to {}", remove.join(", ")));
    }
    (!what.is_empty()).then(|| Cmd {
        label: format!("{repo}#{number}: {}", what.join("; ")),
        args: list,
        ..Cmd::default()
    })
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
        (Act::Ready, Item::Ticket(t)) if t.draft && t.state == "OPEN" => (
            "Mark ready for review:",
            args(&["pr", "ready", &t.number.to_string(), "-R", repo]),
        ),
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
        ..Cmd::default()
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
    let mut guard = None;
    let list = match (form.act, &form.item) {
        (Act::Comment, Some(Item::Ticket(t))) => {
            need(0)?;
            stdin = Some(text(0).to_string());
            let kind = if t.head.is_empty() { "issue" } else { "pr" };
            args(&[kind, "comment", &number, &repo, "--body-file=-"])
        }
        (Act::LineComment, Some(Item::Ticket(t))) => {
            need(0)?;
            return review::comment(&t.repo, t.number, &form.extra, text(0));
        }
        (Act::Reply, Some(Item::Ticket(t))) => {
            need(0)?;
            return review::reply(&t.repo, t.number, &form.extra, text(0));
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
            let method = match form.chosen(0) {
                "Merge commit" => "--merge",
                "Squash" => "--squash",
                "Rebase" => "--rebase",
                other => return Err(format!("unknown method {other:?}")),
            };
            // Refuse to merge commits pushed after the list was loaded.
            let head = format!("--match-head-commit={}", t.oid);
            let mut a = args(&["pr", "merge", &number, &repo, method, &head]);
            if form.toggle(1) {
                a.push("--delete-branch".into());
            }
            a
        }
        (Act::Title, Some(Item::Ticket(t))) => {
            let title = need(0)?;
            if title == t.title {
                return Err("nothing changed".into());
            }
            guard = Some(text_guard(&t.repo, t.number, "title", &t.title));
            args(&["pr", "edit", &number, &repo, &format!("--title={title}")])
        }
        (Act::Description, Some(Item::Ticket(t))) => {
            // The editor uses plain newlines.
            if text(0) == t.body.replace("\r\n", "\n") {
                return Err("nothing changed".into());
            }
            let cmd = edit_body(&t.repo, t.number, &t.body, text(0).to_string());
            return Ok(cmd);
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
        guard,
        ..Cmd::default()
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
        let pick =
            |values: &[&str]| -> Vec<Choice> { values.iter().map(|v| choice(v, v)).collect() };
        let mut q = Query {
            tab: Tab::Prs,
            orgs: vec![],
            repos: vec![],
            filters: vec![],
            states: default_states(Tab::Prs),
            cursor: None,
        };
        let repos = Repos {
            login: "me".into(),
            orgs: vec!["a".into()],
            teams: vec!["a/x".into(), "b/y".into()],
            names: vec![],
        };
        let check = |q: &Query, want: &str| {
            let want = format!("is:pr {want} archived:false sort:updated-desc");
            assert_eq!(search_query(q, &repos), want);
        };
        check(&q, "(user:me OR user:a) is:open");
        q.filters = pick(&["author:@me"]);
        check(&q, "author:@me is:open");
        q.orgs = pick(&["a", "b"]);
        q.filters = pick(&["author:@me", MY_TEAMS]);
        q.states = states(Tab::Prs);
        check(
            &q,
            "(user:a OR user:b) (author:@me OR team-review-requested:a/x OR \
             team-review-requested:b/y) (is:open OR is:merged OR (is:closed is:unmerged))",
        );
        q.repos = pick(&["o/r", "o/s"]);
        q.filters.clear();
        q.states.clear();
        check(&q, "(repo:o/r OR repo:o/s)");
    }

    #[test]
    fn approval_follows_decision_then_reviews() {
        let reviews = |decision: Option<&str>, states: &[&str]| Reviews {
            decision: decision.map(String::from),
            reviewers: states
                .iter()
                .map(|s| Reviewer {
                    name: "x".into(),
                    state: s.to_string(),
                })
                .collect(),
            ..Reviews::default()
        };
        for (decision, states, want) in [
            (Some("REVIEW_REQUIRED"), &["APPROVED"][..], "● required"),
            (Some("APPROVED"), &["REQUESTED"], "✓ approved"),
            (None, &["APPROVED", "CHANGES_REQUESTED"], "✗ changes"),
            (None, &["APPROVED", "REQUESTED"], "✓ approved"),
            (None, &["REQUESTED"], "● pending"),
            (None, &["COMMENTED"], "· none"),
        ] {
            assert_eq!(reviews(decision, states).approval(), want);
        }
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

    #[test]
    fn review_requests_are_diffed() {
        let list = |v: &[&str]| -> Vec<String> { v.iter().map(|s| s.to_string()).collect() };
        let old = list(&["al", "o/team"]);
        assert_eq!(request_reviews("o/r", 7, &old, &old), None);
        let cmd = request_reviews("o/r", 7, &old, &list(&["al", "bo", "o/x"])).unwrap();
        assert_eq!(
            cmd.args[2..],
            [
                "7",
                "--repo=o/r",
                "--add-reviewer=bo,o/x",
                "--remove-reviewer=o/team"
            ]
        );
        assert_eq!(
            cmd.label,
            "o/r#7: request review from bo, o/x; withdraw request to o/team"
        );
    }

    #[test]
    fn no_team_left_matches_nothing() {
        let q = Query {
            tab: Tab::Prs,
            orgs: vec![],
            repos: vec![],
            filters: vec![choice("teams", MY_TEAMS)],
            states: vec![],
            cursor: None,
        };
        // Answers without asking GitHub.
        let page = list(&q, &Repos::default()).unwrap();
        assert!(page.items.is_empty() && page.warning.is_some());
    }

    #[test]
    fn shares_fill_a_page() {
        for n in 1..=12 {
            assert!(share(n) <= PAGE && share(n) * n >= PAGE, "{n}");
        }
        assert_eq!(share(1000), 10);
    }

    #[test]
    fn only_open_drafts_become_ready() {
        let Item::Ticket(mut t) = ticket("x") else {
            unreachable!()
        };
        assert_eq!(simple(Act::Ready, &Item::Ticket(t.clone())), None);
        t.draft = true;
        let cmd = simple(Act::Ready, &Item::Ticket(t.clone())).unwrap();
        assert_eq!(cmd.args, ["pr", "ready", "7", "-R", "o/r"]);
        t.state = "CLOSED".into();
        assert_eq!(simple(Act::Ready, &Item::Ticket(t)), None);
    }

    #[test]
    fn text_edits_are_guarded() {
        let Item::Ticket(mut t) = ticket("x") else {
            unreachable!()
        };
        t.title = "old".into();
        t.body = "a\r\nb".into();
        let item = Some(Item::Ticket(t));
        let mut form = Form::new(Tab::Prs, Act::Title, "o/r", item.clone(), "").unwrap();
        assert_eq!(submit(&form), Err("nothing changed".into()));
        set(&mut form, 0, " ");
        assert!(submit(&form).is_err());
        set(&mut form, 0, "--new");
        let cmd = submit(&form).unwrap();
        assert_eq!(cmd.args[3..], ["--repo=o/r", "--title=--new"]);
        assert_eq!(cmd.guard.unwrap().expected, "old");

        let mut form = Form::new(Tab::Prs, Act::Description, "o/r", item, "").unwrap();
        assert_eq!(submit(&form), Err("nothing changed".into()));
        set(&mut form, 0, "a\nc");
        let cmd = submit(&form).unwrap();
        assert_eq!(cmd.stdin.as_deref(), Some("a\nc"));
        assert_eq!(cmd.guard.unwrap().expected, "a\r\nb");
    }

    #[test]
    fn merge_readiness_names_problems_and_methods() {
        let ready = judge(MergeFacts {
            mergeable: "MERGEABLE".into(),
            state: "BLOCKED".into(),
            decision: Some("REVIEW_REQUIRED".into()),
            checks: [
                ("a", "SUCCESS"),
                ("b", "FAILURE"),
                ("c", "QUEUED"),
                ("d", "SKIPPED"),
            ]
            .map(|(n, s)| choice(n, s))
            .to_vec(),
            unresolved: 2,
            merge: true,
            squash: true,
            preferred: "SQUASH".into(),
            ..MergeFacts::default()
        });
        assert_eq!(ready.methods, ["Squash", "Merge commit"]);
        let notes: Vec<_> = ready.notes.iter().map(|n| (n.0, n.1.as_str())).collect();
        assert_eq!(
            notes,
            [
                (true, "no conflicts"),
                (false, "approval is still required"),
                (false, "checks failed: b"),
                (false, "checks running: c"),
                (false, "2 unresolved discussions"),
                (false, "blocked by the branch rules"),
            ]
        );
        assert!(!ready.conflicts);
        let bad = judge(MergeFacts {
            mergeable: "CONFLICTING".into(),
            ..MergeFacts::default()
        });
        assert!(bad.conflicts && bad.methods.is_empty());
    }
}
